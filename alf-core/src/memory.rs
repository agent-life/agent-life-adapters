//! Memory record types for the ALF format.
//!
//! Matches `memory-record.schema.json`. All types support `additionalProperties`
//! via `#[serde(flatten)]` so that unknown fields survive round-trip (§8.2).

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Forward-compatible enum helper
// ---------------------------------------------------------------------------
//
// The ALF spec (§8.2) requires that unknown enum values are preserved on
// round-trip. We implement this as an enum with known variants plus an
// `Unknown(String)` catch-all. The macro below generates the boilerplate
// Serialize/Deserialize implementations.

macro_rules! forward_compatible_enum {
    (
        $(#[$meta:meta])*
        $vis:vis enum $name:ident {
            $(
                $(#[$variant_meta:meta])*
                $variant:ident => $str:literal
            ),+ $(,)?
        }
    ) => {
        $(#[$meta])*
        #[derive(Debug, Clone, PartialEq, Eq, Hash)]
        $vis enum $name {
            $(
                $(#[$variant_meta])*
                $variant,
            )+
            /// A value not in the known set. Preserved on round-trip per §8.2.
            Unknown(String),
        }

        impl ::serde::Serialize for $name {
            fn serialize<S: ::serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                match self {
                    $( Self::$variant => serializer.serialize_str($str), )+
                    Self::Unknown(s) => serializer.serialize_str(s),
                }
            }
        }

        impl<'de> ::serde::Deserialize<'de> for $name {
            fn deserialize<D: ::serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                let s = String::deserialize(deserializer)?;
                Ok(match s.as_str() {
                    $( $str => Self::$variant, )+
                    _ => Self::Unknown(s),
                })
            }
        }

        impl ::std::fmt::Display for $name {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                match self {
                    $( Self::$variant => f.write_str($str), )+
                    Self::Unknown(s) => f.write_str(s),
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Enums
// ---------------------------------------------------------------------------

forward_compatible_enum! {
    /// Cognitive category of a memory record (§3.1).
    ///
    /// Unknown values are treated as `Semantic` for processing purposes
    /// but preserved as-is on round-trip.
    pub enum MemoryType {
        Semantic    => "semantic",
        Episodic    => "episodic",
        Procedural  => "procedural",
        Preference  => "preference",
        Summary     => "summary",
    }
}

impl MemoryType {
    /// Returns the effective type for processing when the value is unknown.
    /// Per spec §8.2, unknown types are treated as `semantic`.
    pub fn effective(&self) -> &Self {
        match self {
            Self::Unknown(_) => &Self::Semantic,
            other => other,
        }
    }
}

forward_compatible_enum! {
    /// Lifecycle status of a memory record.
    ///
    /// Unknown values are treated as `Active` for processing purposes
    /// but preserved as-is on round-trip.
    pub enum MemoryStatus {
        Active      => "active",
        Superseded  => "superseded",
        Archived    => "archived",
        Deleted     => "deleted",
    }
}

impl MemoryStatus {
    /// Returns the effective status for processing when the value is unknown.
    /// Per spec §8.2, unknown statuses are treated as `active`.
    pub fn effective(&self) -> &Self {
        match self {
            Self::Unknown(_) => &Self::Active,
            other => other,
        }
    }
}

forward_compatible_enum! {
    /// How a memory was created (§3.1.3).
    pub enum ExtractionMethod {
        AgentWritten  => "agent_written",
        LlmExtracted  => "llm_extracted",
        UserAuthored  => "user_authored",
        Migrated      => "migrated",
    }
}

forward_compatible_enum! {
    /// Entity type for extracted entity references (§3.1.5).
    ///
    /// Unknown values are treated as `Other` for processing purposes.
    pub enum EntityType {
        Person       => "person",
        Organization => "organization",
        Project      => "project",
        Location     => "location",
        Tool         => "tool",
        Service      => "service",
        Other        => "other",
    }
}

forward_compatible_enum! {
    /// Who computed an embedding vector (§3.1.6).
    pub enum EmbeddingSource {
        Runtime       => "runtime",
        SyncService   => "sync_service",
        ImportAdapter => "import_adapter",
    }
}

// ---------------------------------------------------------------------------
// Structs
// ---------------------------------------------------------------------------

/// A single memory record in the ALF format.
///
/// Memory records are stored as JSONL (one record per line) within time-based
/// partition files. See §3.1 of the ALF specification and
/// `memory-record.schema.json`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MemoryRecord {
    /// Globally unique, time-sortable identifier (UUID v7).
    pub id: Uuid,

    /// The agent this memory belongs to.
    pub agent_id: Uuid,

    /// The memory text. Plain text or Markdown.
    pub content: String,

    /// Cognitive category.
    pub memory_type: MemoryType,

    /// Where and how this memory was created.
    pub source: SourceProvenance,

    /// Temporal context.
    pub temporal: TemporalMetadata,

    /// Lifecycle status.
    pub status: MemoryStatus,

    /// Scoping namespace. Default: `"default"`.
    pub namespace: String,

    /// Runtime-defined category for adapter round-trip fidelity.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,

    /// ID of the memory record this one replaces (§3.1.8).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub supersedes: Option<Uuid>,

    /// Confidence score from the source runtime (0.0–1.0).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub confidence: Option<f64>,

    /// Extracted entities referenced by this memory (§3.1.5).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub entities: Vec<EntityReference>,

    /// User or agent-defined tags for filtering.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,

    /// Pre-computed embedding vectors (§3.1.6).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub embeddings: Vec<Embedding>,

    /// Typed links to other memory records (§3.1.12).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub related_records: Vec<RelatedRecord>,

    /// Original runtime-specific representation for lossless round-trip.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub raw_source_format: Option<serde_json::Value>,

    /// Unknown fields preserved for forward compatibility (§8.2).
    #[serde(flatten)]
    pub extra: HashMap<String, serde_json::Value>,
}

/// Records where and how a memory was created (§3.1.3).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SourceProvenance {
    /// Which agent framework produced this memory.
    pub runtime: String,

    /// Version of the source runtime.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runtime_version: Option<String>,

    /// Native storage location or mechanism.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub origin: Option<String>,

    /// Path to the original file if extracted from a file.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub origin_file: Option<String>,

    /// How the memory was created.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extraction_method: Option<ExtractionMethod>,

    /// Session identifier from the source runtime.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,

    /// Specific interaction or message identifier.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub interaction_id: Option<String>,

    /// Identity layer version active when this memory was created (§3.1.10).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub identity_version: Option<u32>,

    /// Unknown fields preserved for forward compatibility.
    #[serde(flatten)]
    pub extra: HashMap<String, serde_json::Value>,
}

/// Temporal context for memory consolidation and conflict resolution (§3.1.4).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TemporalMetadata {
    /// When the memory record was created in the neutral format.
    pub created_at: DateTime<Utc>,

    /// Last modification time.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<DateTime<Utc>>,

    /// When the underlying fact or event was originally observed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub observed_at: Option<DateTime<Utc>>,

    /// Start of temporal validity window.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub valid_from: Option<DateTime<Utc>>,

    /// End of temporal validity window. `None` means "still current".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub valid_until: Option<DateTime<Utc>>,

    /// Last access time for relevance scoring and memory decay.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_accessed_at: Option<DateTime<Utc>>,

    /// Number of times this memory has been accessed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub access_count: Option<u64>,

    /// Unknown fields preserved for forward compatibility.
    #[serde(flatten)]
    pub extra: HashMap<String, serde_json::Value>,
}

/// A named entity referenced by a memory record (§3.1.5).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EntityReference {
    /// Entity name.
    pub name: String,

    /// Entity type.
    #[serde(rename = "type")]
    pub entity_type: EntityType,

    /// The entity's role in the context of this memory.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,

    /// Unknown fields preserved for forward compatibility.
    #[serde(flatten)]
    pub extra: HashMap<String, serde_json::Value>,
}

/// A pre-computed embedding vector tagged by model (§3.1.6).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Embedding {
    /// Embedding model identifier in `provider/model` format.
    pub model: String,

    /// Vector dimensionality.
    pub dimensions: u32,

    /// The embedding vector.
    pub vector: Vec<f64>,

    /// When this embedding was computed.
    pub computed_at: DateTime<Utc>,

    /// Who computed this embedding.
    pub source: EmbeddingSource,

    /// Unknown fields preserved for forward compatibility.
    #[serde(flatten)]
    pub extra: HashMap<String, serde_json::Value>,
}

/// A typed link to another memory record (§3.1.12).
///
/// Relation types are free-form strings. Well-known values:
/// `caused_by`, `caused`, `contradicts`, `elaborates_on`, `derived_from`,
/// `related_to`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RelatedRecord {
    /// ID of the related memory record. Dangling references are tolerated.
    pub id: Uuid,

    /// The type of relationship (free-form string).
    pub relation: String,

    /// Unknown fields preserved for forward compatibility.
    #[serde(flatten)]
    pub extra: HashMap<String, serde_json::Value>,
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use pretty_assertions::assert_eq;

    /// Helper: create a minimal valid memory record with only required fields.
    fn minimal_record() -> MemoryRecord {
        let now = Utc.with_ymd_and_hms(2026, 1, 15, 10, 30, 0).unwrap();
        MemoryRecord {
            id: Uuid::now_v7(),
            agent_id: Uuid::new_v4(),
            content: "User prefers dark mode.".into(),
            memory_type: MemoryType::Preference,
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
                observed_at: None,
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

    /// Helper: create a fully populated memory record with all optional fields.
    fn full_record() -> MemoryRecord {
        let now = Utc.with_ymd_and_hms(2026, 1, 15, 10, 30, 0).unwrap();
        let earlier = Utc.with_ymd_and_hms(2026, 1, 14, 8, 0, 0).unwrap();
        let agent_id = Uuid::new_v4();
        let related_id = Uuid::new_v4();

        MemoryRecord {
            id: Uuid::now_v7(),
            agent_id,
            content: "Acme Corp's fiscal year ends March 31.".into(),
            memory_type: MemoryType::Semantic,
            source: SourceProvenance {
                runtime: "openclaw".into(),
                runtime_version: Some("0.4.2".into()),
                origin: Some("daily_log".into()),
                origin_file: Some("logs/daily/2026-01-14.md".into()),
                extraction_method: Some(ExtractionMethod::AgentWritten),
                session_id: Some("sess_abc123".into()),
                interaction_id: Some("msg_456".into()),
                identity_version: Some(3),
                extra: HashMap::new(),
            },
            temporal: TemporalMetadata {
                created_at: now,
                updated_at: Some(now),
                observed_at: Some(earlier),
                valid_from: Some(earlier),
                valid_until: None,
                last_accessed_at: Some(now),
                access_count: Some(5),
                extra: HashMap::new(),
            },
            status: MemoryStatus::Active,
            namespace: "default".into(),
            category: Some("core".into()),
            supersedes: Some(Uuid::new_v4()),
            confidence: Some(0.95),
            entities: vec![
                EntityReference {
                    name: "Acme Corp".into(),
                    entity_type: EntityType::Organization,
                    role: Some("employer".into()),
                    extra: HashMap::new(),
                },
                EntityReference {
                    name: "Jane".into(),
                    entity_type: EntityType::Person,
                    role: Some("colleague".into()),
                    extra: HashMap::new(),
                },
            ],
            tags: vec!["finance".into(), "calendar".into()],
            embeddings: vec![Embedding {
                model: "openai/text-embedding-3-small".into(),
                dimensions: 4,
                vector: vec![0.1, 0.2, 0.3, 0.4],
                computed_at: now,
                source: EmbeddingSource::Runtime,
                extra: HashMap::new(),
            }],
            related_records: vec![RelatedRecord {
                id: related_id,
                relation: "elaborates_on".into(),
                extra: HashMap::new(),
            }],
            raw_source_format: Some(serde_json::json!({
                "original_line": "- Acme Corp fiscal year ends March 31"
            })),
            extra: HashMap::new(),
        }
    }

    #[test]
    fn minimal_record_round_trip() {
        let record = minimal_record();
        let json = serde_json::to_string(&record).unwrap();
        let deserialized: MemoryRecord = serde_json::from_str(&json).unwrap();
        assert_eq!(record, deserialized);
    }

    #[test]
    fn full_record_round_trip() {
        let record = full_record();
        let json = serde_json::to_string(&record).unwrap();
        let deserialized: MemoryRecord = serde_json::from_str(&json).unwrap();
        assert_eq!(record, deserialized);
    }

    #[test]
    fn minimal_record_omits_optional_fields() {
        let record = minimal_record();
        let value: serde_json::Value = serde_json::to_value(&record).unwrap();
        let obj = value.as_object().unwrap();

        // Required fields present
        assert!(obj.contains_key("id"));
        assert!(obj.contains_key("agent_id"));
        assert!(obj.contains_key("content"));
        assert!(obj.contains_key("memory_type"));
        assert!(obj.contains_key("source"));
        assert!(obj.contains_key("temporal"));
        assert!(obj.contains_key("status"));
        assert!(obj.contains_key("namespace"));

        // Optional fields absent
        assert!(!obj.contains_key("category"));
        assert!(!obj.contains_key("supersedes"));
        assert!(!obj.contains_key("confidence"));
        assert!(!obj.contains_key("entities"));
        assert!(!obj.contains_key("tags"));
        assert!(!obj.contains_key("embeddings"));
        assert!(!obj.contains_key("related_records"));
        assert!(!obj.contains_key("raw_source_format"));
    }

    #[test]
    fn unknown_fields_preserved_on_round_trip() {
        // Simulate a record from a future ALF version with extra fields
        let json = serde_json::json!({
            "id": "019462a0-0000-7000-8000-000000000000",
            "agent_id": "550e8400-e29b-41d4-a716-446655440000",
            "content": "Test memory",
            "memory_type": "semantic",
            "source": {
                "runtime": "openclaw",
                "future_source_field": "should survive"
            },
            "temporal": {
                "created_at": "2026-01-15T10:30:00Z",
                "future_temporal_field": 42
            },
            "status": "active",
            "namespace": "default",
            "future_top_level_field": { "nested": true }
        });

        let record: MemoryRecord = serde_json::from_value(json.clone()).unwrap();

        // Unknown top-level field captured in extra
        assert_eq!(
            record.extra.get("future_top_level_field"),
            Some(&serde_json::json!({"nested": true}))
        );

        // Unknown source field captured in source.extra
        assert_eq!(
            record.source.extra.get("future_source_field"),
            Some(&serde_json::json!("should survive"))
        );

        // Unknown temporal field captured in temporal.extra
        assert_eq!(
            record.temporal.extra.get("future_temporal_field"),
            Some(&serde_json::json!(42))
        );

        // Round-trip preserves all unknown fields
        let serialized = serde_json::to_value(&record).unwrap();
        assert_eq!(
            serialized.get("future_top_level_field"),
            Some(&serde_json::json!({"nested": true}))
        );
        assert_eq!(
            serialized["source"].get("future_source_field"),
            Some(&serde_json::json!("should survive"))
        );
        assert_eq!(
            serialized["temporal"].get("future_temporal_field"),
            Some(&serde_json::json!(42))
        );
    }

    #[test]
    fn unknown_enum_values_preserved() {
        let json = serde_json::json!({
            "id": "019462a0-0000-7000-8000-000000000000",
            "agent_id": "550e8400-e29b-41d4-a716-446655440000",
            "content": "Test",
            "memory_type": "future_cognitive_type",
            "source": { "runtime": "openclaw" },
            "temporal": { "created_at": "2026-01-15T10:30:00Z" },
            "status": "future_status",
            "namespace": "default"
        });

        let record: MemoryRecord = serde_json::from_value(json).unwrap();

        // Unknown values are captured
        assert_eq!(
            record.memory_type,
            MemoryType::Unknown("future_cognitive_type".into())
        );
        assert_eq!(
            record.status,
            MemoryStatus::Unknown("future_status".into())
        );

        // Effective types fall back to defaults
        assert_eq!(*record.memory_type.effective(), MemoryType::Semantic);
        assert_eq!(*record.status.effective(), MemoryStatus::Active);

        // Round-trip preserves the original string values
        let value = serde_json::to_value(&record).unwrap();
        assert_eq!(value["memory_type"], "future_cognitive_type");
        assert_eq!(value["status"], "future_status");
    }

    #[test]
    fn unknown_entity_type_preserved() {
        let json = serde_json::json!({
            "name": "Skynet",
            "type": "artificial_intelligence",
            "role": "antagonist"
        });

        let entity: EntityReference = serde_json::from_value(json).unwrap();
        assert_eq!(
            entity.entity_type,
            EntityType::Unknown("artificial_intelligence".into())
        );

        let value = serde_json::to_value(&entity).unwrap();
        assert_eq!(value["type"], "artificial_intelligence");
    }

    #[test]
    fn unknown_extraction_method_preserved() {
        let json = serde_json::json!({
            "runtime": "openclaw",
            "extraction_method": "auto_summarized"
        });

        let source: SourceProvenance = serde_json::from_value(json).unwrap();
        assert_eq!(
            source.extraction_method,
            Some(ExtractionMethod::Unknown("auto_summarized".into()))
        );

        let value = serde_json::to_value(&source).unwrap();
        assert_eq!(value["extraction_method"], "auto_summarized");
    }

    #[test]
    fn entity_reference_type_field_renamed() {
        // The schema uses "type" but Rust reserves that keyword.
        // We use `entity_type` in Rust, renamed to "type" in JSON.
        let entity = EntityReference {
            name: "Acme".into(),
            entity_type: EntityType::Organization,
            role: None,
            extra: HashMap::new(),
        };

        let value = serde_json::to_value(&entity).unwrap();
        assert!(value.get("type").is_some());
        assert!(value.get("entity_type").is_none());
    }

    #[test]
    fn enum_display() {
        assert_eq!(MemoryType::Semantic.to_string(), "semantic");
        assert_eq!(MemoryType::Unknown("custom".into()).to_string(), "custom");
        assert_eq!(MemoryStatus::Active.to_string(), "active");
        assert_eq!(EntityType::Person.to_string(), "person");
    }

    #[test]
    fn embedding_round_trip() {
        let now = Utc::now();
        let embedding = Embedding {
            model: "openai/text-embedding-3-small".into(),
            dimensions: 1536,
            vector: vec![0.1; 1536],
            computed_at: now,
            source: EmbeddingSource::Runtime,
            extra: HashMap::new(),
        };

        let json = serde_json::to_string(&embedding).unwrap();
        let deserialized: Embedding = serde_json::from_str(&json).unwrap();
        assert_eq!(embedding, deserialized);
    }

    #[test]
    fn related_record_round_trip() {
        let related = RelatedRecord {
            id: Uuid::new_v4(),
            relation: "caused_by".into(),
            extra: HashMap::new(),
        };

        let json = serde_json::to_string(&related).unwrap();
        let deserialized: RelatedRecord = serde_json::from_str(&json).unwrap();
        assert_eq!(related, deserialized);
    }
}
