//! Extract memory entries from ZeroClaw's shared `brain.db`.
//!
//! `brain.db` (`<install>/data/memory/brain.db`) is a store shared by every
//! agent in the install, partitioned by `agent_id`. This module reads **one
//! agent's slice** (`WHERE agent_id = ?`) and maps each row to an ALF
//! `MemoryRecord`, preserving every native column in `raw_source_format` so a
//! ZeroClaw→ZeroClaw restore is lossless. Embedding BLOBs are decoded
//! best-effort; when the format is unreadable the record is exported without an
//! embedding vector.

use std::collections::HashMap;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use rusqlite::Connection;
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
/// Auto-save key prefixes on the real store (the old `assistant_autosave_`
/// matched nothing — capture plan §2).
const AUTO_SAVE_PREFIXES: &[&str] = &["user_msg_", "assistant_resp_"];

// ---------------------------------------------------------------------------
// Internal row representation
// ---------------------------------------------------------------------------

/// Raw row from the real `memories` table (all native columns).
struct MemoryRow {
    id: String,
    key: String,
    content: String,
    category: String,
    embedding: Option<Vec<u8>>,
    created_at: String,
    updated_at: String,
    session_id: Option<String>,
    namespace: Option<String>,
    importance: Option<f64>,
    superseded_by: Option<String>,
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Extract one agent's memory slice from an open `brain.db` connection.
///
/// `zc_agent_id` is the ZeroClaw `agents.id` the slice is filtered by;
/// `alf_agent_id` becomes each record's ALF `agent_id`. Returns an empty vec
/// when the `memories` table is absent (lazy store) or the agent has no rows.
pub fn records_from_conn(
    conn: &Connection,
    config: &ZeroClawConfig,
    alf_agent_id: Uuid,
    zc_agent_id: &str,
    runtime_version: Option<&str>,
) -> Result<Vec<MemoryRecord>> {
    let rows = read_agent_rows(conn, zc_agent_id)?;

    // Supersession inverse map (WP4.1 §8.4). brain.db stores a FORWARD pointer
    // (`superseded_by` = the native id of the row that REPLACED this one); the
    // ALF `supersedes` field is the BACKWARD pointer (the record this one
    // replaces). For a row T superseded by native id S, the record with native
    // id S must carry `supersedes = T's ALF id`. Build native_S -> native_T,
    // then resolve T's native id to its ALF uuid per row.
    let mut superseding_to_superseded: HashMap<String, String> = HashMap::new();
    for row in &rows {
        if let Some(s) = row.superseded_by.as_deref().filter(|v| !v.is_empty()) {
            superseding_to_superseded.insert(s.to_string(), row.id.clone());
        }
    }

    let mut records = Vec::with_capacity(rows.len());
    for row in &rows {
        let supersedes = superseding_to_superseded
            .get(&row.id)
            .map(|superseded_native| row_alf_id(superseded_native, alf_agent_id));
        records.push(map_row_to_record(
            row,
            alf_agent_id,
            config,
            runtime_version,
            supersedes,
        )?);
    }
    // Rows already come back ordered by created_at ASC.
    Ok(records)
}

/// ALF record id for a brain.db row's native id: the native UUID when it
/// parses, else a DETERMINISTIC v5 fallback (a random one would churn every
/// export — WP4.1 §8.3). Shared by the row mapper and the supersession map so
/// both resolve a native id to the same ALF uuid.
fn row_alf_id(native_id: &str, alf_agent_id: Uuid) -> Uuid {
    Uuid::parse_str(native_id).unwrap_or_else(|_| {
        Uuid::new_v5(
            &crate::markdown_parser::ZEROCLAW_NS,
            format!("brain-id:{alf_agent_id}:{native_id}").as_bytes(),
        )
    })
}

// ---------------------------------------------------------------------------
// Database reading
// ---------------------------------------------------------------------------

fn read_agent_rows(conn: &Connection, zc_agent_id: &str) -> Result<Vec<MemoryRow>> {
    // A future/alternate backend may lack the table — return empty, don't error.
    let table_exists: bool = conn
        .prepare("SELECT name FROM sqlite_master WHERE type='table' AND name='memories'")?
        .exists([])?;
    if !table_exists {
        return Ok(Vec::new());
    }

    let mut stmt = conn.prepare(
        "SELECT id, key, content, category, embedding, created_at, updated_at, \
                session_id, namespace, importance, superseded_by \
         FROM memories WHERE agent_id = ?1 ORDER BY created_at ASC",
    )?;

    let rows = stmt
        .query_map([zc_agent_id], |row| {
            Ok(MemoryRow {
                id: row.get(0)?,
                key: row.get(1)?,
                content: row.get(2)?,
                category: row.get(3)?,
                embedding: row.get(4)?,
                created_at: row.get(5)?,
                updated_at: row.get(6)?,
                session_id: row.get(7)?,
                namespace: row.get(8)?,
                importance: row.get(9)?,
                superseded_by: row.get(10)?,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()
        .context("Failed to read memories table")?;

    Ok(rows)
}

// ---------------------------------------------------------------------------
// Row → MemoryRecord mapping
// ---------------------------------------------------------------------------

fn map_row_to_record(
    row: &MemoryRow,
    alf_agent_id: Uuid,
    config: &ZeroClawConfig,
    runtime_version: Option<&str>,
    supersedes: Option<Uuid>,
) -> Result<MemoryRecord> {
    let id = row_alf_id(&row.id, alf_agent_id);
    let created_at = parse_timestamp(&row.created_at);
    let updated_at = if row.updated_at.is_empty() {
        None
    } else {
        Some(parse_timestamp(&row.updated_at))
    };
    let is_auto_save = AUTO_SAVE_PREFIXES.iter().any(|p| row.key.starts_with(p));
    let memory_type = classify_category(&row.category);
    let namespace = row
        .namespace
        .clone()
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| "default".to_string());

    let extraction_method = ExtractionMethod::AgentWritten;

    let mut embeddings = Vec::new();
    if let Some(ref blob) = row.embedding {
        if let Some(emb) = try_parse_embedding(blob, config, created_at) {
            embeddings.push(emb);
        }
    }

    let mut tags = vec![row.category.clone(), RUNTIME.to_string()];
    if is_auto_save {
        tags.push("auto_save".to_string());
    }

    // Supersession (WP4.1 §8.4): a non-null `superseded_by` marks THIS row as
    // superseded (it was replaced). The ALF `supersedes` field points the other
    // way — at the record THIS one replaced — and is supplied by the caller's
    // inverse map (a row T whose `superseded_by` names this row's native id).
    // The native forward pointer is preserved verbatim in `raw_source_format`
    // for lossless restore.
    let status = match row.superseded_by.as_deref() {
        Some(v) if !v.is_empty() => MemoryStatus::Superseded,
        _ => MemoryStatus::Active,
    };

    // Lossless stash: every native scalar column, so restore reconstructs the
    // row exactly (timestamps verbatim, importance/session_id/namespace/
    // superseded_by preserved) regardless of the ALF-semantic mapping above.
    let raw_source = serde_json::json!({
        "key": row.key,
        "category": row.category,
        "created_at": row.created_at,
        "updated_at": row.updated_at,
        "session_id": row.session_id,
        "namespace": row.namespace,
        "importance": row.importance,
        "superseded_by": row.superseded_by,
    });

    Ok(MemoryRecord {
        id,
        agent_id: alf_agent_id,
        content: row.content.clone(),
        memory_type,
        source: SourceProvenance {
            runtime: RUNTIME.to_string(),
            runtime_version: runtime_version.map(|s| s.to_string()),
            origin: Some("sqlite".to_string()),
            origin_file: None,
            extraction_method: Some(extraction_method),
            session_id: row.session_id.clone(),
            interaction_id: None,
            identity_version: None,
            extra: HashMap::new(),
        },
        temporal: TemporalMetadata {
            created_at,
            updated_at,
            observed_at: None,
            valid_from: None,
            valid_until: None,
            last_accessed_at: None,
            access_count: None,
            extra: HashMap::new(),
        },
        status,
        namespace,
        category: Some(row.category.clone()),
        supersedes,
        confidence: row.importance,
        entities: Vec::new(),
        tags,
        embeddings,
        related_records: Vec::new(),
        raw_source_format: Some(raw_source),
        extra: HashMap::new(),
    })
}

// ---------------------------------------------------------------------------
// Classification (real taxonomy — capture plan D8)
// ---------------------------------------------------------------------------

/// Map a ZeroClaw `memories.category` to an ALF `MemoryType`. `credentials` is
/// synced verbatim as `Semantic` — ALF is framework-neutral on secrets in
/// framework memory (capture plan §10.8); the `credentials` category tag on the
/// record keeps such rows identifiable.
fn classify_category(category: &str) -> MemoryType {
    match category.to_lowercase().as_str() {
        "core" => MemoryType::Semantic,
        "episodic" => MemoryType::Episodic,
        "procedure" => MemoryType::Procedural,
        "conversation" => MemoryType::Episodic,
        "credentials" => MemoryType::Semantic,
        _ => MemoryType::Semantic,
    }
}

// ---------------------------------------------------------------------------
// Embedding parsing
// ---------------------------------------------------------------------------

/// Try to parse an embedding BLOB as a packed f32 or f64 vector. ZeroClaw stores
/// embeddings as raw byte BLOBs; the format depends on the provider (typically
/// packed little-endian f32). NULL when `embedding_provider = "none"`.
fn try_parse_embedding(
    blob: &[u8],
    config: &ZeroClawConfig,
    timestamp: DateTime<Utc>,
) -> Option<Embedding> {
    if blob.is_empty() {
        return None;
    }

    if blob.len().is_multiple_of(4) {
        let vector: Vec<f64> = blob
            .chunks_exact(4)
            .map(|chunk| {
                let bytes: [u8; 4] = chunk.try_into().unwrap();
                f32::from_le_bytes(bytes) as f64
            })
            .collect();

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

    if blob.len().is_multiple_of(8) {
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

    None
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
// Tests (against the real captured brain.db DDL — capture plan §7)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::brain_db;

    const A: &str = "aaaaaaaa-0000-0000-0000-0000000000a1";
    const B: &str = "bbbbbbbb-0000-0000-0000-0000000000b2";

    fn config() -> ZeroClawConfig {
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

    /// Insert a real-schema `memories` row via the runtime's own trigger path
    /// (so FTS stays consistent). Only the columns the tests exercise are
    /// parameterized; the rest take real defaults.
    #[allow(clippy::too_many_arguments)]
    fn insert(
        conn: &Connection,
        agent_id: &str,
        id: &str,
        key: &str,
        content: &str,
        category: &str,
        created_at: &str,
        superseded_by: Option<&str>,
        importance: f64,
        namespace: &str,
        session_id: Option<&str>,
    ) {
        conn.execute(
            "INSERT INTO memories \
             (id, key, content, category, embedding, created_at, updated_at, \
              session_id, namespace, importance, superseded_by, agent_id) \
             VALUES (?1,?2,?3,?4,NULL,?5,?5,?6,?7,?8,?9,?10)",
            rusqlite::params![
                id,
                key,
                content,
                category,
                created_at,
                session_id,
                namespace,
                importance,
                superseded_by,
                agent_id
            ],
        )
        .unwrap();
    }

    fn db_two_agents() -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let db = brain_db::real_schema_db(dir.path(), &[(A, "agent_a"), (B, "agent_b")]);
        let conn = Connection::open(&db).unwrap();
        insert(
            &conn,
            A,
            "11111111-0000-0000-0000-000000000001",
            "user_pref",
            "User prefers Rust",
            "core",
            "2026-01-15T10:00:00Z",
            None,
            0.9,
            "default",
            None,
        );
        insert(
            &conn,
            A,
            "22222222-0000-0000-0000-000000000002",
            "user_msg_x",
            "hello there",
            "conversation",
            "2026-01-15T11:00:00Z",
            None,
            0.5,
            "default",
            Some("sess-1"),
        );
        insert(
            &conn,
            B,
            "33333333-0000-0000-0000-000000000003",
            "b_secret",
            "b only",
            "core",
            "2026-01-15T12:00:00Z",
            None,
            0.5,
            "default",
            None,
        );
        (dir, db)
    }

    #[test]
    fn maps_real_columns_and_fixes_bug2() {
        let (_dir, db) = db_two_agents();
        let conn = Connection::open(&db).unwrap();
        let recs = records_from_conn(&conn, &config(), Uuid::nil(), A, Some("0.8.2")).unwrap();
        assert_eq!(recs.len(), 2, "only agent A's slice");
        let pref = &recs[0];
        assert_eq!(pref.category.as_deref(), Some("core"));
        assert_eq!(pref.memory_type, MemoryType::Semantic);
        assert_eq!(pref.confidence, Some(0.9));
        // created_at came from the real column, not a nonexistent `timestamp`.
        assert_eq!(
            pref.temporal.created_at,
            parse_timestamp("2026-01-15T10:00:00Z")
        );
    }

    #[test]
    fn agent_filter_prevents_leakage() {
        let (_dir, db) = db_two_agents();
        let conn = Connection::open(&db).unwrap();
        let a = records_from_conn(&conn, &config(), Uuid::nil(), A, None).unwrap();
        let b = records_from_conn(&conn, &config(), Uuid::nil(), B, None).unwrap();
        assert_eq!(a.len(), 2);
        assert_eq!(b.len(), 1);
        assert!(a.iter().all(|r| r.content != "b only"));
        assert_eq!(b[0].content, "b only");
    }

    #[test]
    fn auto_save_prefix_and_conversation_classification() {
        let (_dir, db) = db_two_agents();
        let conn = Connection::open(&db).unwrap();
        let recs = records_from_conn(&conn, &config(), Uuid::nil(), A, None).unwrap();
        let msg = recs.iter().find(|r| r.content == "hello there").unwrap();
        assert_eq!(
            msg.memory_type,
            MemoryType::Episodic,
            "conversation → Episodic"
        );
        assert!(
            msg.tags.contains(&"auto_save".to_string()),
            "user_msg_ → auto_save"
        );
        assert_eq!(msg.source.session_id.as_deref(), Some("sess-1"));
    }

    #[test]
    fn procedure_maps_to_procedural() {
        let dir = tempfile::tempdir().unwrap();
        let db = brain_db::real_schema_db(dir.path(), &[(A, "agent_a")]);
        let conn = Connection::open(&db).unwrap();
        insert(
            &conn,
            A,
            "44444444-0000-0000-0000-000000000004",
            "how_to",
            "step 1",
            "procedure",
            "2026-01-15T10:00:00Z",
            None,
            0.5,
            "default",
            None,
        );
        let recs = records_from_conn(&conn, &config(), Uuid::nil(), A, None).unwrap();
        assert_eq!(recs[0].memory_type, MemoryType::Procedural);
    }

    #[test]
    fn supersedes_points_backward_at_the_replaced_record() {
        // brain.db stores a FORWARD pointer (old.superseded_by = new.id); the
        // ALF `supersedes` field is BACKWARD (new.supersedes = old.id). §8.4.
        let dir = tempfile::tempdir().unwrap();
        let db = brain_db::real_schema_db(dir.path(), &[(A, "agent_a")]);
        let conn = Connection::open(&db).unwrap();
        let old_id = "55555555-0000-0000-0000-000000000005";
        let new_id = "99999999-0000-0000-0000-000000000099";
        // Old row, replaced by the new one.
        insert(
            &conn,
            A,
            old_id,
            "old_fact",
            "outdated",
            "core",
            "2026-01-15T10:00:00Z",
            Some(new_id),
            0.5,
            "default",
            None,
        );
        // The replacing row (created later; ordered after the old one).
        insert(
            &conn,
            A,
            new_id,
            "new_fact",
            "current",
            "core",
            "2026-01-15T11:00:00Z",
            None,
            0.9,
            "default",
            None,
        );

        let recs = records_from_conn(&conn, &config(), Uuid::nil(), A, None).unwrap();
        let old = recs.iter().find(|r| r.id.to_string() == old_id).unwrap();
        let new = recs.iter().find(|r| r.id.to_string() == new_id).unwrap();

        // The OLD record is marked superseded and points at nothing (it does
        // not replace anything); its native forward pointer is preserved raw.
        assert_eq!(old.status, MemoryStatus::Superseded);
        assert_eq!(old.supersedes, None);
        assert_eq!(
            old.raw_source_format.as_ref().unwrap()["superseded_by"],
            serde_json::json!(new_id)
        );
        // The NEW record is active and its `supersedes` points BACK at the old.
        assert_eq!(new.status, MemoryStatus::Active);
        assert_eq!(
            new.supersedes.map(|u| u.to_string()).as_deref(),
            Some(old_id)
        );
    }

    #[test]
    fn credentials_category_synced_verbatim_as_semantic() {
        let dir = tempfile::tempdir().unwrap();
        let db = brain_db::real_schema_db(dir.path(), &[(A, "agent_a")]);
        let conn = Connection::open(&db).unwrap();
        insert(
            &conn,
            A,
            "66666666-0000-0000-0000-000000000006",
            "api_token",
            "sk-FAKE",
            "credentials",
            "2026-01-15T10:00:00Z",
            None,
            0.5,
            "default",
            None,
        );
        let recs = records_from_conn(&conn, &config(), Uuid::nil(), A, None).unwrap();
        assert_eq!(recs[0].memory_type, MemoryType::Semantic);
        assert_eq!(recs[0].category.as_deref(), Some("credentials"));
    }

    #[test]
    fn missing_table_returns_empty() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("empty.db");
        Connection::open(&db).unwrap(); // no tables
        let conn = Connection::open(&db).unwrap();
        let recs = records_from_conn(&conn, &config(), Uuid::nil(), A, None).unwrap();
        assert!(recs.is_empty());
    }

    #[test]
    fn preserves_native_uuid() {
        let dir = tempfile::tempdir().unwrap();
        let db = brain_db::real_schema_db(dir.path(), &[(A, "agent_a")]);
        let conn = Connection::open(&db).unwrap();
        let expected = "a1b2c3d4-e5f6-4a7b-8c9d-0e1f2a3b4c5d";
        insert(
            &conn,
            A,
            expected,
            "k",
            "c",
            "core",
            "2026-01-15T10:00:00Z",
            None,
            0.5,
            "default",
            None,
        );
        let recs = records_from_conn(&conn, &config(), Uuid::nil(), A, None).unwrap();
        assert_eq!(recs[0].id, Uuid::parse_str(expected).unwrap());
    }

    #[test]
    fn embedding_none_provider_skipped() {
        let blob: Vec<u8> = vec![0u8; 128 * 4];
        let mut cfg = config();
        cfg.embedding_provider = "none".into();
        assert!(try_parse_embedding(&blob, &cfg, Utc::now()).is_none());
    }

    #[test]
    fn embedding_extraction_f32() {
        let dims = 128;
        let mut blob = Vec::with_capacity(dims * 4);
        for i in 0..dims {
            blob.extend_from_slice(&((i as f32) * 0.01).to_le_bytes());
        }
        let emb = try_parse_embedding(&blob, &config(), Utc::now()).unwrap();
        assert_eq!(emb.dimensions, 128);
        assert_eq!(emb.model, "openai/text-embedding-3-small");
    }
}
