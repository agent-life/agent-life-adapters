//! Extract memory entries from ZeroClaw's SQLite database.
//!
//! Opens `memory.db` in read-only mode, reads all rows from the `memories`
//! table, and maps each to an ALF `MemoryRecord`. Embedding BLOBs are
//! extracted best-effort — if the BLOB format is unreadable, the record is
//! exported without embeddings.

use std::collections::HashMap;
use std::path::Path;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use rusqlite::{Connection, OpenFlags};
use uuid::Uuid;

use alf_core::{
    Embedding, EmbeddingSource, ExtractionMethod, MemoryRecord, MemoryStatus, MemoryType,
    SourceProvenance, TemporalMetadata,
};

use crate::config_parser::ZeroClawConfig;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

const RUNTIME: &str = "zeroclaw";
const AUTO_SAVE_PREFIX: &str = "assistant_autosave_";

// ---------------------------------------------------------------------------
// Internal row representation
// ---------------------------------------------------------------------------

/// Raw row from the `memories` table.
struct MemoryRow {
    id: String,
    key: String,
    content: String,
    category: String,
    timestamp: String,
    embedding: Option<Vec<u8>>,
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Extract all memory entries from the ZeroClaw SQLite database.
///
/// Opens the database read-only. Returns an empty vec if the database is
/// empty. Returns an error if the database cannot be opened or queried.
pub fn extract_from_sqlite(
    db_path: &Path,
    config: &ZeroClawConfig,
    agent_id: Uuid,
    runtime_version: Option<&str>,
) -> Result<Vec<MemoryRecord>> {
    let conn = Connection::open_with_flags(db_path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .with_context(|| format!("Failed to open ZeroClaw database: {}", db_path.display()))?;

    let rows = read_all_rows(&conn)?;
    let mut records = Vec::with_capacity(rows.len());

    for row in rows {
        let record = map_row_to_record(row, agent_id, config, runtime_version)?;
        records.push(record);
    }

    // Sort by created_at ascending
    records.sort_by(|a, b| a.temporal.created_at.cmp(&b.temporal.created_at));

    Ok(records)
}

// ---------------------------------------------------------------------------
// Database reading
// ---------------------------------------------------------------------------

fn read_all_rows(conn: &Connection) -> Result<Vec<MemoryRow>> {
    // Check if the memories table exists
    let table_exists: bool = conn
        .prepare("SELECT name FROM sqlite_master WHERE type='table' AND name='memories'")?
        .exists([])?;

    if !table_exists {
        return Ok(Vec::new());
    }

    let mut stmt = conn.prepare(
        "SELECT id, key, content, category, timestamp, embedding FROM memories ORDER BY timestamp ASC",
    )?;

    let rows = stmt
        .query_map([], |row| {
            Ok(MemoryRow {
                id: row.get(0)?,
                key: row.get(1)?,
                content: row.get(2)?,
                category: row.get(3)?,
                timestamp: row.get(4)?,
                embedding: row.get(5)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()
        .context("Failed to read memories table")?;

    Ok(rows)
}

// ---------------------------------------------------------------------------
// Row → MemoryRecord mapping
// ---------------------------------------------------------------------------

fn map_row_to_record(
    row: MemoryRow,
    agent_id: Uuid,
    config: &ZeroClawConfig,
    runtime_version: Option<&str>,
) -> Result<MemoryRecord> {
    let id = Uuid::parse_str(&row.id).unwrap_or_else(|_| Uuid::new_v4());
    let created_at = parse_timestamp(&row.timestamp);
    let is_auto_save = row.key.starts_with(AUTO_SAVE_PREFIX);
    let (memory_type, namespace) = classify_category(&row.category);

    let extraction_method = if is_auto_save {
        ExtractionMethod::AgentWritten
    } else {
        ExtractionMethod::AgentWritten
    };

    let mut embeddings = Vec::new();
    if let Some(blob) = row.embedding {
        if let Some(emb) = try_parse_embedding(&blob, config, created_at) {
            embeddings.push(emb);
        }
    }

    let mut tags = vec![row.category.clone(), RUNTIME.to_string()];
    if is_auto_save {
        tags.push("auto_save".to_string());
    }

    let raw_source = serde_json::json!({
        "key": row.key,
        "category": row.category,
    });

    Ok(MemoryRecord {
        id,
        agent_id,
        content: row.content,
        memory_type,
        source: SourceProvenance {
            runtime: RUNTIME.to_string(),
            runtime_version: runtime_version.map(|s| s.to_string()),
            origin: Some("sqlite".to_string()),
            origin_file: None,
            extraction_method: Some(extraction_method),
            session_id: None,
            interaction_id: None,
            identity_version: None,
            extra: HashMap::new(),
        },
        temporal: TemporalMetadata {
            created_at,
            updated_at: None,
            observed_at: None,
            valid_from: None,
            valid_until: None,
            last_accessed_at: None,
            access_count: None,
            extra: HashMap::new(),
        },
        status: MemoryStatus::Active,
        namespace,
        category: Some(row.category),
        supersedes: None,
        confidence: None,
        entities: Vec::new(),
        tags,
        embeddings,
        related_records: Vec::new(),
        raw_source_format: Some(raw_source),
        extra: HashMap::new(),
    })
}

// ---------------------------------------------------------------------------
// Classification
// ---------------------------------------------------------------------------

/// Map a ZeroClaw `MemoryCategory` string to ALF `MemoryType` and namespace.
fn classify_category(category: &str) -> (MemoryType, String) {
    match category.to_lowercase().as_str() {
        "core" => (MemoryType::Semantic, "core".to_string()),
        "daily" => (MemoryType::Episodic, "daily".to_string()),
        "conversation" => (MemoryType::Episodic, "conversation".to_string()),
        other => {
            // Custom categories: custom:label → namespace "custom:{label}"
            let ns = if let Some(label) = other.strip_prefix("custom:") {
                format!("custom:{label}")
            } else {
                format!("custom:{other}")
            };
            (MemoryType::Semantic, ns)
        }
    }
}

// ---------------------------------------------------------------------------
// Embedding parsing
// ---------------------------------------------------------------------------

/// Try to parse an embedding BLOB as a packed f32 or f64 vector.
///
/// ZeroClaw stores embeddings as raw byte BLOBs in SQLite. The format
/// depends on the embedding provider — typically packed little-endian
/// f32 values from OpenAI. We try f32 first (most common), then f64.
fn try_parse_embedding(
    blob: &[u8],
    config: &ZeroClawConfig,
    timestamp: DateTime<Utc>,
) -> Option<Embedding> {
    if blob.is_empty() {
        return None;
    }

    // Try as packed f32 (4 bytes each)
    if blob.len() % 4 == 0 {
        let vector: Vec<f64> = blob
            .chunks_exact(4)
            .map(|chunk| {
                let bytes: [u8; 4] = chunk.try_into().unwrap();
                f32::from_le_bytes(bytes) as f64
            })
            .collect();

        // Sanity check: reasonable embedding dimensions (64–4096)
        if vector.len() >= 64 && vector.len() <= 4096 {
            let model = match config.embedding_provider.as_str() {
                "openai" => "openai/text-embedding-3-small".to_string(),
                "none" | "noop" => return None,
                other => other.to_string(),
            };
            return Some(Embedding {
                model,
                dimensions: vector.len() as u32,
                vector,
                computed_at: timestamp,
                source: EmbeddingSource::Runtime,
                extra: HashMap::new(),
            });
        }
    }

    // Try as packed f64 (8 bytes each)
    if blob.len() % 8 == 0 {
        let vector: Vec<f64> = blob
            .chunks_exact(8)
            .map(|chunk| {
                let bytes: [u8; 8] = chunk.try_into().unwrap();
                f64::from_le_bytes(bytes)
            })
            .collect();

        if vector.len() >= 64 && vector.len() <= 4096 {
            let model = format!("unknown/{}", config.embedding_provider);
            return Some(Embedding {
                model,
                dimensions: vector.len() as u32,
                vector,
                computed_at: timestamp,
                source: EmbeddingSource::Runtime,
                extra: HashMap::new(),
            });
        }
    }

    None // Unrecognized format — skip silently
}

// ---------------------------------------------------------------------------
// Timestamp parsing
// ---------------------------------------------------------------------------

/// Parse an RFC 3339 timestamp string, falling back to current time.
fn parse_timestamp(ts: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(ts)
        .map(|dt| dt.with_timezone(&Utc))
        .unwrap_or_else(|_| Utc::now())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn create_test_db(path: &Path) -> Connection {
        let conn = Connection::open(path).unwrap();
        conn.execute_batch(
            "CREATE TABLE memories (
                id TEXT PRIMARY KEY,
                key TEXT NOT NULL,
                content TEXT NOT NULL,
                category TEXT NOT NULL,
                timestamp TEXT NOT NULL,
                embedding BLOB
            );",
        )
        .unwrap();
        conn
    }

    fn test_config() -> ZeroClawConfig {
        ZeroClawConfig {
            memory_backend: crate::config_parser::MemoryBackend::Sqlite,
            auto_save: true,
            embedding_provider: "openai".into(),
            vector_weight: 0.7,
            keyword_weight: 0.3,
            identity_format: crate::config_parser::IdentityFormat::OpenClaw,
            aieos_path: None,
            aieos_inline: None,
            secrets_encrypt: true,
            credential_hints: Vec::new(),
            raw_toml: String::new(),
        }
    }

    #[test]
    fn extract_basic_entries() {
        let tmp = tempfile::tempdir().unwrap();
        let db_path = tmp.path().join("memory.db");
        let conn = create_test_db(&db_path);
        let agent_id = Uuid::new_v4();

        conn.execute(
            "INSERT INTO memories VALUES (?1, ?2, ?3, ?4, ?5, NULL)",
            rusqlite::params![
                "550e8400-e29b-41d4-a716-446655440000",
                "user_timezone",
                "User is in America/Los_Angeles",
                "core",
                "2026-01-15T10:30:00Z",
            ],
        ).unwrap();

        conn.execute(
            "INSERT INTO memories VALUES (?1, ?2, ?3, ?4, ?5, NULL)",
            rusqlite::params![
                "550e8400-e29b-41d4-a716-446655440001",
                "daily_observation",
                "Reviewed migration plan today",
                "daily",
                "2026-01-15T14:00:00Z",
            ],
        ).unwrap();

        drop(conn);

        let config = test_config();
        let records = extract_from_sqlite(&db_path, &config, agent_id, Some("0.1.0")).unwrap();

        assert_eq!(records.len(), 2);
        assert_eq!(records[0].memory_type, MemoryType::Semantic);
        assert_eq!(records[0].namespace, "core");
        assert_eq!(records[1].memory_type, MemoryType::Episodic);
        assert_eq!(records[1].namespace, "daily");
    }

    #[test]
    fn auto_save_entries_tagged() {
        let tmp = tempfile::tempdir().unwrap();
        let db_path = tmp.path().join("memory.db");
        let conn = create_test_db(&db_path);
        let agent_id = Uuid::new_v4();

        conn.execute(
            "INSERT INTO memories VALUES (?1, ?2, ?3, ?4, ?5, NULL)",
            rusqlite::params![
                "550e8400-e29b-41d4-a716-446655440010",
                "assistant_autosave_msg_1",
                "What is the weather today?",
                "conversation",
                "2026-01-15T10:00:00Z",
            ],
        ).unwrap();
        drop(conn);

        let config = test_config();
        let records = extract_from_sqlite(&db_path, &config, agent_id, None).unwrap();

        assert_eq!(records.len(), 1);
        assert_eq!(
            records[0].source.extraction_method,
            Some(ExtractionMethod::AgentWritten)
        );
        assert!(records[0].tags.contains(&"auto_save".to_string()));
        assert_eq!(records[0].namespace, "conversation");
    }

    #[test]
    fn preserves_native_uuid() {
        let tmp = tempfile::tempdir().unwrap();
        let db_path = tmp.path().join("memory.db");
        let conn = create_test_db(&db_path);
        let agent_id = Uuid::new_v4();
        let expected_id = "a1b2c3d4-e5f6-7890-abcd-ef1234567890";

        conn.execute(
            "INSERT INTO memories VALUES (?1, ?2, ?3, ?4, ?5, NULL)",
            rusqlite::params![expected_id, "k", "content", "core", "2026-01-01T00:00:00Z"],
        ).unwrap();
        drop(conn);

        let config = test_config();
        let records = extract_from_sqlite(&db_path, &config, agent_id, None).unwrap();

        assert_eq!(records[0].id, Uuid::parse_str(expected_id).unwrap());
    }

    #[test]
    fn empty_database() {
        let tmp = tempfile::tempdir().unwrap();
        let db_path = tmp.path().join("memory.db");
        create_test_db(&db_path);

        let config = test_config();
        let records = extract_from_sqlite(&db_path, &config, Uuid::new_v4(), None).unwrap();
        assert!(records.is_empty());
    }

    #[test]
    fn missing_table_returns_empty() {
        let tmp = tempfile::tempdir().unwrap();
        let db_path = tmp.path().join("memory.db");
        Connection::open(&db_path).unwrap(); // empty database, no tables

        let config = test_config();
        let records = extract_from_sqlite(&db_path, &config, Uuid::new_v4(), None).unwrap();
        assert!(records.is_empty());
    }

    #[test]
    fn custom_category_mapping() {
        let (ty, ns) = classify_category("core");
        assert_eq!(ty, MemoryType::Semantic);
        assert_eq!(ns, "core");

        let (ty, ns) = classify_category("conversation");
        assert_eq!(ty, MemoryType::Episodic);
        assert_eq!(ns, "conversation");

        let (ty, ns) = classify_category("custom:procedures");
        assert_eq!(ty, MemoryType::Semantic);
        assert_eq!(ns, "custom:procedures");

        let (ty, ns) = classify_category("unknown_bucket");
        assert_eq!(ty, MemoryType::Semantic);
        assert_eq!(ns, "custom:unknown_bucket");
    }

    #[test]
    fn embedding_extraction_f32() {
        // Construct a 128-dim f32 embedding
        let dims = 128;
        let mut blob = Vec::with_capacity(dims * 4);
        for i in 0..dims {
            let val = (i as f32) * 0.01;
            blob.extend_from_slice(&val.to_le_bytes());
        }

        let config = test_config();
        let ts = Utc::now();
        let emb = try_parse_embedding(&blob, &config, ts).unwrap();

        assert_eq!(emb.dimensions, 128);
        assert_eq!(emb.model, "openai/text-embedding-3-small");
        assert_eq!(emb.vector.len(), 128);
        assert!((emb.vector[1] - 0.01).abs() < 1e-5);
    }

    #[test]
    fn embedding_none_provider_skipped() {
        let dims = 128;
        let blob: Vec<u8> = vec![0u8; dims * 4];
        let mut config = test_config();
        config.embedding_provider = "none".into();

        let result = try_parse_embedding(&blob, &config, Utc::now());
        assert!(result.is_none());
    }
}