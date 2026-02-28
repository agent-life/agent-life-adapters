//! Manifest types for ALF snapshots and deltas, plus attachment index types.
//!
//! Matches `manifest.schema.json`, `delta-manifest.schema.json`, and
//! `attachments.schema.json`. All types support `additionalProperties` via
//! `#[serde(flatten)]` so that unknown fields survive round-trip (§8.2).

use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use uuid::Uuid;

// forward_compatible_enum! is available via #[macro_use] on mod memory in lib.rs

// ===========================================================================
// Snapshot Manifest (manifest.schema.json)
// ===========================================================================

/// Top-level manifest for a complete ALF snapshot (`.alf` file).
///
/// Contains format version, agent metadata, runtime hints, sync cursor,
/// and layer inventory with partition details. See §4.2 of the ALF
/// specification.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Manifest {
    /// Semantic version of the ALF format (e.g., `"1.0.0"`).
    pub alf_version: String,

    /// When this snapshot was created.
    pub created_at: DateTime<Utc>,

    /// Core identifying information about the agent.
    pub agent: AgentMetadata,

    /// Layer inventory with partition details.
    pub layers: LayerInventory,

    /// Runtime hints (model, provider, context window).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runtime_hints: Option<RuntimeHints>,

    /// Sync state of this snapshot.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sync: Option<SyncCursor>,

    /// List of runtime identifiers whose raw files are preserved in `raw/`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub raw_sources: Vec<String>,

    /// Integrity checksum in `algorithm:hex` format (e.g., `"sha256:abcdef..."`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub checksum: Option<String>,

    /// Unknown fields preserved for forward compatibility.
    #[serde(flatten)]
    pub extra: HashMap<String, serde_json::Value>,
}

/// Core identifying information about the agent.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentMetadata {
    /// Globally unique agent identifier (UUID).
    pub id: Uuid,

    /// Display name of the agent.
    pub name: String,

    /// The agent framework this snapshot was exported from.
    pub source_runtime: String,

    /// Version of the source runtime.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_runtime_version: Option<String>,

    /// Unknown fields preserved for forward compatibility.
    #[serde(flatten)]
    pub extra: HashMap<String, serde_json::Value>,
}

/// Factual metadata about the model the agent ran on (§3.2.5).
///
/// These are hints, not prescriptive requirements.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RuntimeHints {
    /// The model the agent spent most of its life on (`provider/model`).
    pub primary_model: String,

    /// The most recently used model.
    pub last_model: String,

    /// The LLM provider.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,

    /// Context window size in tokens.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_window: Option<u64>,

    /// Free-form notes about the runtime configuration.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,

    /// Unknown fields preserved for forward compatibility.
    #[serde(flatten)]
    pub extra: HashMap<String, serde_json::Value>,
}

/// Tracks the sync state of a snapshot (§4.3.1).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SyncCursor {
    /// Monotonic sequence number of the most recent sync operation.
    pub last_sequence: u64,

    /// Timestamp of the most recent sync operation.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_sync_at: Option<DateTime<Utc>>,

    /// Unknown fields preserved for forward compatibility.
    #[serde(flatten)]
    pub extra: HashMap<String, serde_json::Value>,
}

// ---------------------------------------------------------------------------
// Layer Inventory
// ---------------------------------------------------------------------------

/// Inventory of all data layers present in the snapshot.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LayerInventory {
    /// Identity layer info.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub identity: Option<IdentityLayerInfo>,

    /// Principals layer info.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub principals: Option<PrincipalsLayerInfo>,

    /// Credentials layer info.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub credentials: Option<CredentialsLayerInfo>,

    /// Memory layer info with partition inventory.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub memory: Option<MemoryInventory>,

    /// Attachments layer info.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attachments: Option<AttachmentsLayerInfo>,

    /// Unknown fields preserved for forward compatibility.
    #[serde(flatten)]
    pub extra: HashMap<String, serde_json::Value>,
}

/// Identity layer presence and version in the snapshot.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IdentityLayerInfo {
    /// Current identity version number.
    pub version: u32,

    /// Path to the identity JSON file within the archive.
    pub file: String,

    /// Unknown fields preserved for forward compatibility.
    #[serde(flatten)]
    pub extra: HashMap<String, serde_json::Value>,
}

/// Principals layer presence and count in the snapshot.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PrincipalsLayerInfo {
    /// Number of principals.
    pub count: u32,

    /// Path to the principals JSON file within the archive.
    pub file: String,

    /// Unknown fields preserved for forward compatibility.
    #[serde(flatten)]
    pub extra: HashMap<String, serde_json::Value>,
}

/// Credentials layer presence and count in the snapshot.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CredentialsLayerInfo {
    /// Number of credential records.
    pub count: u32,

    /// Path to the credentials JSON file within the archive.
    pub file: String,

    /// Unknown fields preserved for forward compatibility.
    #[serde(flatten)]
    pub extra: HashMap<String, serde_json::Value>,
}

/// Attachments layer presence and artifact breakdown in the snapshot.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AttachmentsLayerInfo {
    /// Total number of artifact references (Tier 2 + Tier 3).
    pub count: u32,

    /// Path to the attachments JSON file within the archive.
    pub file: String,

    /// Number of Tier 2 artifacts included in the archive under `artifacts/`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub included_count: Option<u32>,

    /// Total size of Tier 2 (included) artifacts in bytes.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub included_size_bytes: Option<u64>,

    /// Number of Tier 3 artifacts cataloged by reference only.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub referenced_count: Option<u32>,

    /// Total size of Tier 3 (reference-only) artifacts in bytes.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub referenced_size_bytes: Option<u64>,

    /// Unknown fields preserved for forward compatibility.
    #[serde(flatten)]
    pub extra: HashMap<String, serde_json::Value>,
}

// ---------------------------------------------------------------------------
// Memory Inventory
// ---------------------------------------------------------------------------

/// Metadata about the memory layer, including the partition inventory.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MemoryInventory {
    /// Total number of memory records across all partitions.
    pub record_count: u64,

    /// Path to the memory index file within the archive.
    pub index_file: String,

    /// Ordered list of time-based memory partitions (§4.1.1).
    pub partitions: Vec<MemoryPartitionInfo>,

    /// Whether any partition contains pre-computed embeddings.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub has_embeddings: Option<bool>,

    /// Whether raw source files are preserved in the `raw/` directory.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub has_raw_source: Option<bool>,

    /// Unknown fields preserved for forward compatibility.
    #[serde(flatten)]
    pub extra: HashMap<String, serde_json::Value>,
}

/// A time-based memory partition within the archive (§4.1.1).
///
/// Sealed partitions cover completed time periods and are immutable — they
/// can be cached aggressively by the sync service.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MemoryPartitionInfo {
    /// Path to the JSONL partition file within the archive.
    pub file: String,

    /// Start date of the partition's time range (inclusive).
    pub from: NaiveDate,

    /// End date of the partition's time range (inclusive).
    /// `None` for the current (unsealed) partition.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub to: Option<NaiveDate>,

    /// Number of memory records in this partition.
    pub record_count: u64,

    /// Whether this partition is immutable.
    pub sealed: bool,

    /// Unknown fields preserved for forward compatibility.
    #[serde(flatten)]
    pub extra: HashMap<String, serde_json::Value>,
}

// ===========================================================================
// Attachment Index (attachments.schema.json)
// ===========================================================================

/// Workspace artifact index (§3.1.9).
///
/// This is the top-level JSON object stored in the attachments file
/// referenced by the manifest.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AttachmentsIndex {
    /// The size threshold in bytes used to classify Tier 2 vs. Tier 3.
    /// Default: 102400 (100 KB).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub artifact_size_threshold: Option<u64>,

    /// List of workspace artifact references.
    pub attachments: Vec<AttachmentReference>,

    /// Unknown fields preserved for forward compatibility.
    #[serde(flatten)]
    pub extra: HashMap<String, serde_json::Value>,
}

/// A reference to a workspace artifact (§3.1.9).
///
/// If `archive_path` is `Some(...)` (Tier 2), the file is included in the
/// archive under `artifacts/`. If `archive_path` is `None` (Tier 3), the
/// file is cataloged by reference only.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AttachmentReference {
    /// Unique identifier for this artifact reference.
    pub id: Uuid,

    /// Original filename.
    pub filename: String,

    /// MIME type (e.g., `"image/png"`, `"text/csv"`).
    pub media_type: String,

    /// File size at time of export.
    pub size_bytes: u64,

    /// Integrity hash of the file content.
    pub hash: ContentHash,

    /// Path relative to the runtime workspace where the file was found.
    pub source_path: String,

    /// Path within the ALF archive if included (Tier 2), or `None` (Tier 3).
    pub archive_path: Option<String>,

    /// URL for future online storage. `None` in initial releases.
    pub remote_ref: Option<String>,

    /// Memory record IDs that reference this artifact.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub referenced_by: Vec<Uuid>,

    /// Unknown fields preserved for forward compatibility.
    #[serde(flatten)]
    pub extra: HashMap<String, serde_json::Value>,
}

/// Integrity hash of file content.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContentHash {
    /// Hash algorithm (default: `"sha256"`).
    pub algorithm: String,

    /// Hex-encoded hash value.
    pub value: String,

    /// Unknown fields preserved for forward compatibility.
    #[serde(flatten)]
    pub extra: HashMap<String, serde_json::Value>,
}

// ===========================================================================
// Delta Manifest (delta-manifest.schema.json)
// ===========================================================================

/// Manifest for an incremental sync delta bundle (`.alf-delta` file).
///
/// Contains sync cursors (sequence numbers) and indicates which layers
/// have changes. See §4.3 of the ALF specification.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DeltaManifest {
    /// Semantic version of the ALF format.
    pub alf_version: String,

    /// When this delta was created.
    pub created_at: DateTime<Utc>,

    /// Agent this delta applies to.
    pub agent: DeltaAgentRef,

    /// Sync cursor: base sequence and new sequence.
    pub sync: DeltaSyncCursor,

    /// Which layers have changes in this delta.
    pub changes: ChangeInventory,

    /// Unknown fields preserved for forward compatibility.
    #[serde(flatten)]
    pub extra: HashMap<String, serde_json::Value>,
}

/// Agent reference within a delta manifest.
///
/// Lighter than the full `AgentMetadata` — only `id` is required.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DeltaAgentRef {
    /// Agent identifier. Must match the snapshot this delta applies to.
    pub id: Uuid,

    /// The runtime that produced this delta.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_runtime: Option<String>,

    /// Unknown fields preserved for forward compatibility.
    #[serde(flatten)]
    pub extra: HashMap<String, serde_json::Value>,
}

/// Sync cursor for a delta bundle (§4.3.1).
///
/// Identifies the base state this delta builds on and the new state
/// after applying it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DeltaSyncCursor {
    /// Sequence number of the snapshot or last delta this one builds on.
    pub base_sequence: u64,

    /// Sequence number after applying this delta (assigned by server).
    pub new_sequence: u64,

    /// Timestamp corresponding to the base sequence.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_timestamp: Option<DateTime<Utc>>,

    /// Timestamp corresponding to the new sequence.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub new_timestamp: Option<DateTime<Utc>>,

    /// Unknown fields preserved for forward compatibility.
    #[serde(flatten)]
    pub extra: HashMap<String, serde_json::Value>,
}

/// Which layers have changes in this delta.
///
/// Only layers with changes are present in the delta bundle.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChangeInventory {
    /// Present if the identity changed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub identity: Option<IdentityChange>,

    /// Present if any principal profiles changed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub principals: Option<PrincipalsChange>,

    /// Present if credentials changed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub credentials: Option<CredentialsChange>,

    /// Present if memory records were created, updated, or deleted.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub memory: Option<MemoryChange>,

    /// Unknown fields preserved for forward compatibility.
    #[serde(flatten)]
    pub extra: HashMap<String, serde_json::Value>,
}

/// Identity layer change within a delta.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IdentityChange {
    /// Path to the identity JSON file within the delta bundle.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,

    /// New identity version number after this delta.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub new_version: Option<u32>,

    /// Unknown fields preserved for forward compatibility.
    #[serde(flatten)]
    pub extra: HashMap<String, serde_json::Value>,
}

/// Principals layer change within a delta.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PrincipalsChange {
    /// Path to the principals JSON file within the delta bundle.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,

    /// IDs of principals that changed.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub changed_ids: Vec<Uuid>,

    /// Unknown fields preserved for forward compatibility.
    #[serde(flatten)]
    pub extra: HashMap<String, serde_json::Value>,
}

/// Credentials layer change within a delta.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CredentialsChange {
    /// Path to the credentials JSON file within the delta bundle.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,

    /// Unknown fields preserved for forward compatibility.
    #[serde(flatten)]
    pub extra: HashMap<String, serde_json::Value>,
}

/// Memory layer change within a delta.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MemoryChange {
    /// Path to the delta JSONL file within the delta bundle.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,

    /// Number of delta records (creates + updates + deletes).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub record_count: Option<u64>,

    /// Unknown fields preserved for forward compatibility.
    #[serde(flatten)]
    pub extra: HashMap<String, serde_json::Value>,
}

// ---------------------------------------------------------------------------
// Delta operation enum
// ---------------------------------------------------------------------------

forward_compatible_enum! {
    /// The change operation for a memory record within a delta bundle.
    pub enum DeltaOperation {
        Create => "create",
        Update => "update",
        Delete => "delete",
    }
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use pretty_assertions::assert_eq;

    // -- Helpers -----------------------------------------------------------

    fn sample_manifest() -> Manifest {
        let now = Utc.with_ymd_and_hms(2026, 2, 15, 12, 0, 0).unwrap();
        let agent_id = Uuid::new_v4();

        Manifest {
            alf_version: "1.0.0".into(),
            created_at: now,
            agent: AgentMetadata {
                id: agent_id,
                name: "my-agent".into(),
                source_runtime: "openclaw".into(),
                source_runtime_version: Some("0.4.2".into()),
                extra: HashMap::new(),
            },
            layers: LayerInventory {
                identity: Some(IdentityLayerInfo {
                    version: 3,
                    file: "identity.json".into(),
                    extra: HashMap::new(),
                }),
                principals: Some(PrincipalsLayerInfo {
                    count: 1,
                    file: "principals.json".into(),
                    extra: HashMap::new(),
                }),
                credentials: Some(CredentialsLayerInfo {
                    count: 2,
                    file: "credentials.json".into(),
                    extra: HashMap::new(),
                }),
                memory: Some(MemoryInventory {
                    record_count: 150,
                    index_file: "memory/index.json".into(),
                    partitions: vec![
                        MemoryPartitionInfo {
                            file: "memory/2025-Q4.jsonl".into(),
                            from: NaiveDate::from_ymd_opt(2025, 10, 1).unwrap(),
                            to: Some(NaiveDate::from_ymd_opt(2025, 12, 31).unwrap()),
                            record_count: 80,
                            sealed: true,
                            extra: HashMap::new(),
                        },
                        MemoryPartitionInfo {
                            file: "memory/2026-Q1.jsonl".into(),
                            from: NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(),
                            to: None,
                            record_count: 70,
                            sealed: false,
                            extra: HashMap::new(),
                        },
                    ],
                    has_embeddings: Some(true),
                    has_raw_source: Some(true),
                    extra: HashMap::new(),
                }),
                attachments: Some(AttachmentsLayerInfo {
                    count: 5,
                    file: "attachments.json".into(),
                    included_count: Some(3),
                    included_size_bytes: Some(245_000),
                    referenced_count: Some(2),
                    referenced_size_bytes: Some(15_000_000),
                    extra: HashMap::new(),
                }),
                extra: HashMap::new(),
            },
            runtime_hints: Some(RuntimeHints {
                primary_model: "anthropic/claude-sonnet-4-20250514".into(),
                last_model: "anthropic/claude-sonnet-4-20250514".into(),
                provider: Some("anthropic".into()),
                context_window: Some(200_000),
                notes: None,
                extra: HashMap::new(),
            }),
            sync: Some(SyncCursor {
                last_sequence: 42,
                last_sync_at: Some(now),
                extra: HashMap::new(),
            }),
            raw_sources: vec!["openclaw".into()],
            checksum: Some("sha256:abcdef0123456789".into()),
            extra: HashMap::new(),
        }
    }

    fn sample_delta_manifest() -> DeltaManifest {
        let now = Utc.with_ymd_and_hms(2026, 2, 16, 9, 0, 0).unwrap();
        let agent_id = Uuid::new_v4();

        DeltaManifest {
            alf_version: "1.0.0".into(),
            created_at: now,
            agent: DeltaAgentRef {
                id: agent_id,
                source_runtime: Some("openclaw".into()),
                extra: HashMap::new(),
            },
            sync: DeltaSyncCursor {
                base_sequence: 42,
                new_sequence: 0, // assigned by server
                base_timestamp: Some(now),
                new_timestamp: None,
                extra: HashMap::new(),
            },
            changes: ChangeInventory {
                identity: None,
                principals: None,
                credentials: None,
                memory: Some(MemoryChange {
                    file: Some("memory/delta.jsonl".into()),
                    record_count: Some(3),
                    extra: HashMap::new(),
                }),
                extra: HashMap::new(),
            },
            extra: HashMap::new(),
        }
    }

    fn sample_attachments_index() -> AttachmentsIndex {
        AttachmentsIndex {
            artifact_size_threshold: Some(10_485_760),
            attachments: vec![
                AttachmentReference {
                    id: Uuid::new_v4(),
                    filename: "shares_tracker.csv".into(),
                    media_type: "text/csv".into(),
                    size_bytes: 15_234,
                    hash: ContentHash {
                        algorithm: "sha256".into(),
                        value: "a1b2c3d4e5f6".into(),
                        extra: HashMap::new(),
                    },
                    source_path: "workspace/shares_tracker.csv".into(),
                    archive_path: Some("artifacts/shares_tracker.csv".into()),
                    remote_ref: None,
                    referenced_by: vec![Uuid::new_v4()],
                    extra: HashMap::new(),
                },
                AttachmentReference {
                    id: Uuid::new_v4(),
                    filename: "model_weights.bin".into(),
                    media_type: "application/octet-stream".into(),
                    size_bytes: 50_000_000,
                    hash: ContentHash {
                        algorithm: "sha256".into(),
                        value: "f6e5d4c3b2a1".into(),
                        extra: HashMap::new(),
                    },
                    source_path: "workspace/models/weights.bin".into(),
                    archive_path: None, // Tier 3 — reference only
                    remote_ref: None,
                    referenced_by: vec![],
                    extra: HashMap::new(),
                },
            ],
            extra: HashMap::new(),
        }
    }

    // -- Snapshot manifest -------------------------------------------------

    #[test]
    fn manifest_round_trip() {
        let manifest = sample_manifest();
        let json = serde_json::to_string_pretty(&manifest).unwrap();
        let deserialized: Manifest = serde_json::from_str(&json).unwrap();
        assert_eq!(manifest, deserialized);
    }

    #[test]
    fn manifest_minimal() {
        // Only required fields: alf_version, created_at, agent, layers
        let now = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();
        let manifest = Manifest {
            alf_version: "1.0.0".into(),
            created_at: now,
            agent: AgentMetadata {
                id: Uuid::new_v4(),
                name: "test".into(),
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
        };

        let value = serde_json::to_value(&manifest).unwrap();
        let obj = value.as_object().unwrap();

        // Required fields present
        assert!(obj.contains_key("alf_version"));
        assert!(obj.contains_key("created_at"));
        assert!(obj.contains_key("agent"));
        assert!(obj.contains_key("layers"));

        // Optional fields absent
        assert!(!obj.contains_key("runtime_hints"));
        assert!(!obj.contains_key("sync"));
        assert!(!obj.contains_key("raw_sources"));
        assert!(!obj.contains_key("checksum"));

        // Round-trip
        let json = serde_json::to_string(&manifest).unwrap();
        let deserialized: Manifest = serde_json::from_str(&json).unwrap();
        assert_eq!(manifest, deserialized);
    }

    #[test]
    fn manifest_unknown_fields_preserved() {
        let json = serde_json::json!({
            "alf_version": "1.0.0",
            "created_at": "2026-01-01T00:00:00Z",
            "agent": {
                "id": "550e8400-e29b-41d4-a716-446655440000",
                "name": "test",
                "source_runtime": "openclaw",
                "future_agent_field": "preserved"
            },
            "layers": {
                "future_layer": { "count": 10 }
            },
            "future_manifest_field": [1, 2, 3]
        });

        let manifest: Manifest = serde_json::from_value(json).unwrap();

        // Top-level unknown field
        assert_eq!(
            manifest.extra.get("future_manifest_field"),
            Some(&serde_json::json!([1, 2, 3]))
        );

        // Agent unknown field
        assert_eq!(
            manifest.agent.extra.get("future_agent_field"),
            Some(&serde_json::json!("preserved"))
        );

        // Layer inventory unknown field (a future layer type)
        assert_eq!(
            manifest.layers.extra.get("future_layer"),
            Some(&serde_json::json!({"count": 10}))
        );

        // Round-trip preserves all
        let serialized = serde_json::to_value(&manifest).unwrap();
        assert_eq!(
            serialized["future_manifest_field"],
            serde_json::json!([1, 2, 3])
        );
        assert_eq!(
            serialized["agent"]["future_agent_field"],
            serde_json::json!("preserved")
        );
        assert_eq!(
            serialized["layers"]["future_layer"],
            serde_json::json!({"count": 10})
        );
    }

    #[test]
    fn manifest_partition_dates() {
        let manifest = sample_manifest();
        let memory = manifest.layers.memory.unwrap();

        let sealed = &memory.partitions[0];
        assert_eq!(sealed.from, NaiveDate::from_ymd_opt(2025, 10, 1).unwrap());
        assert_eq!(
            sealed.to,
            Some(NaiveDate::from_ymd_opt(2025, 12, 31).unwrap())
        );
        assert!(sealed.sealed);

        let current = &memory.partitions[1];
        assert_eq!(
            current.from,
            NaiveDate::from_ymd_opt(2026, 1, 1).unwrap()
        );
        assert_eq!(current.to, None);
        assert!(!current.sealed);
    }

    // -- Delta manifest ----------------------------------------------------

    #[test]
    fn delta_manifest_round_trip() {
        let delta = sample_delta_manifest();
        let json = serde_json::to_string_pretty(&delta).unwrap();
        let deserialized: DeltaManifest = serde_json::from_str(&json).unwrap();
        assert_eq!(delta, deserialized);
    }

    #[test]
    fn delta_manifest_all_changes() {
        let now = Utc::now();
        let delta = DeltaManifest {
            alf_version: "1.0.0".into(),
            created_at: now,
            agent: DeltaAgentRef {
                id: Uuid::new_v4(),
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
                identity: Some(IdentityChange {
                    file: Some("identity.json".into()),
                    new_version: Some(4),
                    extra: HashMap::new(),
                }),
                principals: Some(PrincipalsChange {
                    file: Some("principals.json".into()),
                    changed_ids: vec![Uuid::new_v4()],
                    extra: HashMap::new(),
                }),
                credentials: Some(CredentialsChange {
                    file: Some("credentials.json".into()),
                    extra: HashMap::new(),
                }),
                memory: Some(MemoryChange {
                    file: Some("memory/delta.jsonl".into()),
                    record_count: Some(7),
                    extra: HashMap::new(),
                }),
                extra: HashMap::new(),
            },
            extra: HashMap::new(),
        };

        let json = serde_json::to_string(&delta).unwrap();
        let deserialized: DeltaManifest = serde_json::from_str(&json).unwrap();
        assert_eq!(delta, deserialized);
    }

    #[test]
    fn delta_manifest_empty_changes() {
        let now = Utc::now();
        let delta = DeltaManifest {
            alf_version: "1.0.0".into(),
            created_at: now,
            agent: DeltaAgentRef {
                id: Uuid::new_v4(),
                source_runtime: None,
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

        let value = serde_json::to_value(&delta).unwrap();
        let changes = value["changes"].as_object().unwrap();

        // No change layers serialized
        assert!(!changes.contains_key("identity"));
        assert!(!changes.contains_key("principals"));
        assert!(!changes.contains_key("credentials"));
        assert!(!changes.contains_key("memory"));
    }

    #[test]
    fn delta_operation_enum() {
        assert_eq!(DeltaOperation::Create.to_string(), "create");
        assert_eq!(DeltaOperation::Update.to_string(), "update");
        assert_eq!(DeltaOperation::Delete.to_string(), "delete");

        // Unknown operation preserved
        let op: DeltaOperation = serde_json::from_str("\"merge\"").unwrap();
        assert_eq!(op, DeltaOperation::Unknown("merge".into()));
        assert_eq!(serde_json::to_string(&op).unwrap(), "\"merge\"");
    }

    // -- Attachments index -------------------------------------------------

    #[test]
    fn attachments_index_round_trip() {
        let index = sample_attachments_index();
        let json = serde_json::to_string_pretty(&index).unwrap();
        let deserialized: AttachmentsIndex = serde_json::from_str(&json).unwrap();
        assert_eq!(index, deserialized);
    }

    #[test]
    fn attachment_tier_classification() {
        let index = sample_attachments_index();

        // First attachment: Tier 2 (has archive_path)
        assert!(index.attachments[0].archive_path.is_some());

        // Second attachment: Tier 3 (archive_path is None)
        assert!(index.attachments[1].archive_path.is_none());
    }

    #[test]
    fn attachment_unknown_fields_preserved() {
        let json = serde_json::json!({
            "id": "550e8400-e29b-41d4-a716-446655440000",
            "filename": "test.txt",
            "media_type": "text/plain",
            "size_bytes": 100,
            "hash": {
                "algorithm": "sha256",
                "value": "abc123",
                "future_hash_field": true
            },
            "source_path": "workspace/test.txt",
            "archive_path": null,
            "remote_ref": null,
            "future_attachment_field": "preserved"
        });

        let attachment: AttachmentReference = serde_json::from_value(json).unwrap();
        assert_eq!(
            attachment.extra.get("future_attachment_field"),
            Some(&serde_json::json!("preserved"))
        );
        assert_eq!(
            attachment.hash.extra.get("future_hash_field"),
            Some(&serde_json::json!(true))
        );

        // Round-trip
        let serialized = serde_json::to_value(&attachment).unwrap();
        assert_eq!(
            serialized["future_attachment_field"],
            serde_json::json!("preserved")
        );
        assert_eq!(serialized["hash"]["future_hash_field"], serde_json::json!(true));
    }

    #[test]
    fn content_hash_round_trip() {
        let hash = ContentHash {
            algorithm: "sha256".into(),
            value: "deadbeef01234567".into(),
            extra: HashMap::new(),
        };

        let json = serde_json::to_string(&hash).unwrap();
        let deserialized: ContentHash = serde_json::from_str(&json).unwrap();
        assert_eq!(hash, deserialized);
    }
}