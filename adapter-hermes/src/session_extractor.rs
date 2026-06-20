//! Read Hermes `state.db` sessions into ALF episodic records (export side).
//!
//! One episodic `MemoryRecord` per session. `content` is a readable transcript
//! (for search / the browser); `raw_source_format` is the full structured
//! session — `{ "session": {<all cols>}, "messages": [{<all cols>}…] }` — the
//! lossless input for [`crate::session_rebuilder`]. We also capture the source
//! DB's own DDL so restore can replay the exact schema (version-agnostic; see
//! the Phase 0 findings doc). The DB is opened READ-ONLY; the binary is never
//! archived (D7) — these records are the only session representation.

use std::collections::HashMap;
use std::path::Path;

use anyhow::{Context, Result};
use chrono::{DateTime, TimeZone, Utc};
use rusqlite::types::ValueRef;
use rusqlite::{Connection, OpenFlags};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use uuid::Uuid;

use alf_core::{
    ExtractionMethod, MemoryRecord, MemoryStatus, MemoryType, SourceProvenance, TemporalMetadata,
};

const RUNTIME: &str = "hermes";

/// The captured `state.db` schema — replayed verbatim on rebuild. Stored as a
/// raw-source sidecar so it travels with the archive and through deltas.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StateDbSchema {
    pub schema_version: i64,
    /// `CREATE` statements from the source DB, minus FTS5 auto-shadow tables and
    /// `sqlite_sequence` (SQLite/FTS5 recreate those).
    pub ddl: Vec<String>,
}

/// What [`extract_sessions`] returns: the per-session records and the schema
/// needed to rebuild the DB on restore.
pub struct SessionExport {
    pub records: Vec<MemoryRecord>,
    pub schema: StateDbSchema,
}

/// Capture just the `state.db` schema (DDL + version), opening read-only.
///
/// Used by both export and `export --dry-run` to write the schema sidecar
/// without the cost of materializing every session record.
pub fn capture_state_schema(db_path: &Path) -> Result<StateDbSchema> {
    let conn = Connection::open_with_flags(db_path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .with_context(|| format!("Failed to open Hermes state.db: {}", db_path.display()))?;
    schema_from_conn(&conn)
}

fn schema_from_conn(conn: &Connection) -> Result<StateDbSchema> {
    Ok(StateDbSchema {
        schema_version: conn
            .query_row("SELECT version FROM schema_version LIMIT 1", [], |r| {
                r.get(0)
            })
            .unwrap_or(-1),
        ddl: capture_ddl(conn)?,
    })
}

/// Extract every session from `state.db` as an episodic record, plus the DDL.
pub fn extract_sessions(
    db_path: &Path,
    agent_id: Uuid,
    runtime_version: Option<&str>,
) -> Result<SessionExport> {
    let conn = Connection::open_with_flags(db_path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .with_context(|| format!("Failed to open Hermes state.db: {}", db_path.display()))?;

    let schema = schema_from_conn(&conn)?;

    let sessions = read_rows(&conn, "SELECT * FROM sessions ORDER BY started_at, id")?;
    let messages = read_rows(&conn, "SELECT * FROM messages ORDER BY session_id, id")?;

    // Bucket messages by session_id once (avoids O(sessions×messages)).
    let mut by_session: HashMap<String, Vec<Value>> = HashMap::new();
    for m in messages {
        if let Some(sid) = m.get("session_id").and_then(Value::as_str) {
            by_session
                .entry(sid.to_string())
                .or_default()
                .push(Value::Object(m));
        }
    }

    let mut records = Vec::with_capacity(sessions.len());
    for sess in sessions {
        let native_id = sess
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let msgs = by_session.remove(&native_id).unwrap_or_default();
        records.push(make_record(
            sess,
            &native_id,
            msgs,
            agent_id,
            runtime_version,
        ));
    }

    Ok(SessionExport { records, schema })
}

fn make_record(
    session: Map<String, Value>,
    native_id: &str,
    messages: Vec<Value>,
    agent_id: Uuid,
    runtime_version: Option<&str>,
) -> MemoryRecord {
    // Native ids are `YYYYMMDD_HHMMSS_<hex>`, not UUIDs → derive a stable UUIDv5
    // so an immutable ended session produces no delta churn across syncs.
    let id = Uuid::new_v5(
        &alf_core::ids::ALF_ID_NAMESPACE,
        format!("hermes-session:{native_id}").as_bytes(),
    );
    let source_platform = session
        .get("source")
        .and_then(Value::as_str)
        .unwrap_or("unknown")
        .to_string();
    let title = session
        .get("title")
        .and_then(Value::as_str)
        .map(str::to_string);
    let started = session.get("started_at").and_then(Value::as_f64);
    let created_at = started.map(unix_to_dt).unwrap_or_else(Utc::now);

    let content = build_transcript(title.as_deref(), &source_platform, &messages);

    let raw_source_format = serde_json::json!({
        "session": Value::Object(session),
        "messages": messages,
    });

    let mut tags = vec![
        RUNTIME.to_string(),
        "session".to_string(),
        source_platform.clone(),
    ];
    tags.dedup();

    MemoryRecord {
        id,
        agent_id,
        content,
        memory_type: MemoryType::Episodic,
        source: SourceProvenance {
            runtime: RUNTIME.to_string(),
            runtime_version: runtime_version.map(str::to_string),
            origin: Some("state_db".to_string()),
            origin_file: None,
            extraction_method: Some(ExtractionMethod::AgentWritten),
            session_id: Some(native_id.to_string()),
            interaction_id: None,
            identity_version: None,
            extra: HashMap::new(),
        },
        temporal: TemporalMetadata {
            created_at,
            updated_at: None,
            observed_at: started.map(unix_to_dt),
            valid_from: None,
            valid_until: None,
            last_accessed_at: None,
            access_count: None,
            extra: HashMap::new(),
        },
        status: MemoryStatus::Active,
        namespace: format!("session:{source_platform}"),
        category: Some(source_platform),
        supersedes: None,
        confidence: None,
        entities: Vec::new(),
        tags,
        embeddings: Vec::new(),
        related_records: Vec::new(),
        raw_source_format: Some(raw_source_format),
        extra: HashMap::new(),
    }
}

/// A readable transcript for search/browser (the structured truth is in
/// `raw_source_format`). One `role: content` line per message.
fn build_transcript(title: Option<&str>, source: &str, messages: &[Value]) -> String {
    let mut out = String::new();
    if let Some(t) = title {
        out.push_str(&format!("# {t}\n"));
    }
    out.push_str(&format!("_source: {source}_\n\n"));
    for m in messages {
        let role = m.get("role").and_then(Value::as_str).unwrap_or("?");
        if let Some(c) = m.get("content").and_then(Value::as_str) {
            if !c.trim().is_empty() {
                out.push_str(&format!("{role}: {c}\n"));
            }
        }
        if let Some(tn) = m.get("tool_name").and_then(Value::as_str) {
            out.push_str(&format!("{role} → tool {tn}\n"));
        }
    }
    out.trim_end().to_string()
}

fn unix_to_dt(secs: f64) -> DateTime<Utc> {
    let whole = secs.trunc() as i64;
    let nanos = ((secs.fract()) * 1e9) as u32;
    Utc.timestamp_opt(whole, nanos)
        .single()
        .unwrap_or_else(Utc::now)
}

// ---------------------------------------------------------------------------
// Generic row → JSON (shared shape with the rebuilder's insert path)
// ---------------------------------------------------------------------------

/// Read all rows of a query into JSON objects preserving SQLite types.
pub(crate) fn read_rows(conn: &Connection, sql: &str) -> Result<Vec<Map<String, Value>>> {
    let mut stmt = conn.prepare(sql)?;
    let cols: Vec<String> = stmt.column_names().iter().map(|s| s.to_string()).collect();
    let mut rows = stmt.query([])?;
    let mut out = Vec::new();
    while let Some(row) = rows.next()? {
        let mut obj = Map::new();
        for (i, name) in cols.iter().enumerate() {
            obj.insert(name.clone(), valueref_to_json(row.get_ref(i)?));
        }
        out.push(obj);
    }
    Ok(out)
}

fn valueref_to_json(v: ValueRef<'_>) -> Value {
    match v {
        ValueRef::Null => Value::Null,
        ValueRef::Integer(i) => serde_json::json!(i),
        ValueRef::Real(f) => serde_json::json!(f),
        ValueRef::Text(t) => serde_json::json!(String::from_utf8_lossy(t)),
        // No BLOBs occur in sessions/messages; tag defensively if one appears.
        ValueRef::Blob(b) => serde_json::json!({ "$blob_len": b.len() }),
    }
}

/// Capture the source DB's own `CREATE` statements, dropping the entries SQLite
/// and FTS5 recreate automatically (FTS shadow tables + `sqlite_sequence`).
pub(crate) fn capture_ddl(conn: &Connection) -> Result<Vec<String>> {
    let mut stmt = conn.prepare("SELECT name, sql FROM sqlite_master WHERE sql IS NOT NULL")?;
    let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
    let mut out = Vec::new();
    for row in rows {
        let (name, sql) = row?;
        if name == "sqlite_sequence" || is_fts_shadow(&name) {
            continue;
        }
        out.push(sql);
    }
    Ok(out)
}

/// FTS5 shadow tables created implicitly by `CREATE VIRTUAL TABLE … fts5`.
fn is_fts_shadow(name: &str) -> bool {
    for base in ["messages_fts", "messages_fts_trigram"] {
        for suf in ["_data", "_idx", "_content", "_docsize", "_config"] {
            if name == format!("{base}{suf}") {
                return true;
            }
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a minimal Hermes-like state.db (the real schema is exercised by the
    /// testkit; this is enough for unit-level extraction assertions).
    fn seed(path: &Path) {
        let c = Connection::open(path).unwrap();
        c.execute_batch(
            "CREATE TABLE schema_version (version INTEGER NOT NULL);
             INSERT INTO schema_version VALUES (16);
             CREATE TABLE sessions (id TEXT PRIMARY KEY, source TEXT NOT NULL, title TEXT,
                 parent_session_id TEXT, started_at REAL NOT NULL, ended_at REAL);
             CREATE TABLE messages (id INTEGER PRIMARY KEY AUTOINCREMENT, session_id TEXT NOT NULL,
                 role TEXT NOT NULL, content TEXT, tool_name TEXT, timestamp REAL NOT NULL);
             CREATE VIRTUAL TABLE messages_fts USING fts5(content);",
        )
        .unwrap();
        c.execute(
            "INSERT INTO sessions (id, source, title, started_at, ended_at) VALUES ('20260101_120000_aa','cli','Hi',1767268800.0,1767272400.0)",
            [],
        ).unwrap();
        c.execute("INSERT INTO messages (session_id, role, content, timestamp) VALUES ('20260101_120000_aa','user','hello there',1767268800.0)", []).unwrap();
        c.execute("INSERT INTO messages (session_id, role, content, timestamp) VALUES ('20260101_120000_aa','assistant','hi back',1767268801.0)", []).unwrap();
    }

    #[test]
    fn extracts_one_record_per_session_with_full_raw() {
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("state.db");
        seed(&db);

        let agent = Uuid::new_v4();
        let out = extract_sessions(&db, agent, Some("0.1.9")).unwrap();
        assert_eq!(out.records.len(), 1);
        assert_eq!(out.schema.schema_version, 16);
        assert!(out
            .schema
            .ddl
            .iter()
            .any(|d| d.contains("CREATE TABLE sessions")));
        // FTS virtual table kept; no shadow tables captured.
        assert!(out
            .schema
            .ddl
            .iter()
            .any(|d| d.to_uppercase().contains("VIRTUAL TABLE")));
        assert!(!out
            .schema
            .ddl
            .iter()
            .any(|d| d.contains("messages_fts_data")));

        let r = &out.records[0];
        assert_eq!(r.memory_type, MemoryType::Episodic);
        assert_eq!(r.namespace, "session:cli");
        assert_eq!(r.source.session_id.as_deref(), Some("20260101_120000_aa"));
        assert!(r.content.contains("user: hello there"));
        let raw = r.raw_source_format.as_ref().unwrap();
        assert_eq!(raw["messages"].as_array().unwrap().len(), 2);
        assert_eq!(raw["session"]["source"], "cli");
        assert!(r.embeddings.is_empty());
    }

    #[test]
    fn session_id_stable_across_extracts() {
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("state.db");
        seed(&db);
        let a = extract_sessions(&db, Uuid::new_v4(), None).unwrap();
        let b = extract_sessions(&db, Uuid::new_v4(), None).unwrap();
        assert_eq!(a.records[0].id, b.records[0].id);
    }
}
