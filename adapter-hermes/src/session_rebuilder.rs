//! Rebuild Hermes `state.db` from ALF session records (import side).
//!
//! The load-bearing greenfield path, validated by the Phase 0 spike. Strategy:
//! replay the captured source DDL (so we are schema-version-agnostic), INSERT
//! sessions then messages from each record's `raw_source_format` with explicit
//! `messages.id` (keeps FTS rowids aligned), and let Hermes's own triggers
//! repopulate `messages_fts` + `messages_fts_trigram`. A real Hermes then opens
//! the result read-write (it runs an FTS5 write-probe on open).

use std::fs;
use std::path::Path;

use anyhow::{Context, Result};
use rusqlite::types::Value as SqlValue;
use rusqlite::Connection;
use serde_json::{Map, Value};

use alf_core::MemoryRecord;

use crate::session_extractor::StateDbSchema;

/// Rebuild `state.db` at `db_path` from the session records + captured schema.
///
/// Only records carrying a `raw_source_format.session` object are used (others
/// — e.g. from a cross-runtime archive — are skipped; the caller warns). Any
/// pre-existing DB at the path (and its WAL/SHM sidecars) is removed first.
/// Returns the number of sessions written.
pub fn rebuild_state_db(
    db_path: &Path,
    records: &[MemoryRecord],
    schema: &StateDbSchema,
) -> Result<usize> {
    for p in [
        db_path.to_path_buf(),
        with_suffix(db_path, "-wal"),
        with_suffix(db_path, "-shm"),
    ] {
        let _ = fs::remove_file(p);
    }
    if let Some(parent) = db_path.parent() {
        fs::create_dir_all(parent)?;
    }

    let conn = Connection::open(db_path)
        .with_context(|| format!("Failed to create state.db: {}", db_path.display()))?;

    // Replay DDL ordered so dependencies exist first.
    for stmt in order_ddl(&schema.ddl) {
        conn.execute_batch(&stmt)
            .with_context(|| format!("replaying state.db DDL failed:\n{stmt}"))?;
    }
    if schema.schema_version >= 0 {
        // schema_version may be empty after DDL replay; set it.
        conn.execute("DELETE FROM schema_version", []).ok();
        conn.execute(
            "INSERT INTO schema_version (version) VALUES (?1)",
            [schema.schema_version],
        )
        .context("setting schema_version")?;
    }

    let mut sessions_written = 0usize;
    for rec in records {
        let Some(raw) = rec.raw_source_format.as_ref() else {
            continue;
        };
        let Some(session) = raw.get("session").and_then(Value::as_object) else {
            continue;
        };
        insert_row(&conn, "sessions", session)?;
        sessions_written += 1;
        if let Some(messages) = raw.get("messages").and_then(Value::as_array) {
            for m in messages {
                if let Some(obj) = m.as_object() {
                    insert_row(&conn, "messages", obj)?;
                }
            }
        }
    }
    Ok(sessions_written)
}

/// Order DDL so replay dependencies are satisfied:
/// real tables → fts5 virtual tables → indexes → triggers.
fn order_ddl(ddl: &[String]) -> Vec<String> {
    let mut tables = Vec::new();
    let mut virtuals = Vec::new();
    let mut indexes = Vec::new();
    let mut triggers = Vec::new();
    for s in ddl {
        let u = s.trim_start().to_uppercase();
        if u.starts_with("CREATE VIRTUAL TABLE") {
            virtuals.push(s.clone());
        } else if u.starts_with("CREATE TABLE") {
            tables.push(s.clone());
        } else if u.starts_with("CREATE TRIGGER") {
            triggers.push(s.clone());
        } else {
            indexes.push(s.clone());
        }
    }
    [tables, virtuals, indexes, triggers].concat()
}

/// Build and run an INSERT from a JSON row object, binding values back to
/// native SQLite types. Column affinity resolves int-vs-real on REAL columns.
fn insert_row(conn: &Connection, table: &str, row: &Map<String, Value>) -> Result<()> {
    let cols: Vec<&String> = row.keys().collect();
    if cols.is_empty() {
        return Ok(());
    }
    let placeholders: Vec<String> = (1..=cols.len()).map(|i| format!("?{i}")).collect();
    let sql = format!(
        "INSERT INTO {table} ({}) VALUES ({})",
        cols.iter()
            .map(|s| s.as_str())
            .collect::<Vec<_>>()
            .join(", "),
        placeholders.join(", ")
    );
    let values: Vec<SqlValue> = cols.iter().map(|c| json_to_value(&row[*c])).collect();
    let params: Vec<&dyn rusqlite::ToSql> =
        values.iter().map(|v| v as &dyn rusqlite::ToSql).collect();
    conn.execute(&sql, params.as_slice())
        .with_context(|| format!("insert into {table}"))?;
    Ok(())
}

fn json_to_value(v: &Value) -> SqlValue {
    match v {
        Value::Null => SqlValue::Null,
        Value::Bool(b) => SqlValue::Integer(*b as i64),
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                SqlValue::Integer(i)
            } else {
                SqlValue::Real(n.as_f64().unwrap_or(0.0))
            }
        }
        Value::String(s) => SqlValue::Text(s.clone()),
        // Arrays/objects (and the defensive $blob tag) shouldn't reach a column;
        // store as JSON text rather than failing the rebuild.
        other => SqlValue::Text(other.to_string()),
    }
}

fn with_suffix(path: &Path, suffix: &str) -> std::path::PathBuf {
    let mut s = path.as_os_str().to_owned();
    s.push(suffix);
    std::path::PathBuf::from(s)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session_extractor::extract_sessions;
    use rusqlite::Connection;
    use uuid::Uuid;

    fn seed(path: &Path) {
        let c = Connection::open(path).unwrap();
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
        c.execute("INSERT INTO sessions (id, source, title, started_at) VALUES ('20260101_120000_aa','cli','Hi',1767268800.0)", []).unwrap();
        c.execute("INSERT INTO sessions (id, source, title, parent_session_id, started_at) VALUES ('20260101_130000_bb','telegram','Hi2','20260101_120000_aa',1767272400.0)", []).unwrap();
        c.execute("INSERT INTO messages (session_id, role, content, timestamp) VALUES ('20260101_120000_aa','user','search for retry markers',1767268800.0)", []).unwrap();
        c.execute("INSERT INTO messages (session_id, role, content, timestamp) VALUES ('20260101_130000_bb','assistant','WAL contention',1767272401.0)", []).unwrap();
    }

    #[test]
    fn round_trip_sessions_and_fts() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("source.db");
        seed(&src);

        let export = extract_sessions(&src, Uuid::new_v4(), None).unwrap();
        let dst = tmp.path().join("rebuilt.db");
        let n = rebuild_state_db(&dst, &export.records, &export.schema).unwrap();
        assert_eq!(n, 2);

        let c = Connection::open(&dst).unwrap();
        // Sessions + lineage preserved.
        let sessions: i64 = c
            .query_row("SELECT COUNT(*) FROM sessions", [], |r| r.get(0))
            .unwrap();
        assert_eq!(sessions, 2);
        let parent: String = c
            .query_row(
                "SELECT parent_session_id FROM sessions WHERE id='20260101_130000_bb'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(parent, "20260101_120000_aa");
        // FTS repopulated by the trigger.
        let hits: i64 = c
            .query_row(
                "SELECT COUNT(*) FROM messages_fts WHERE messages_fts MATCH 'retry'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(hits, 1);
        // schema_version set.
        let ver: i64 = c
            .query_row("SELECT version FROM schema_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(ver, 16);
    }

    #[test]
    fn skips_records_without_session_payload() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("source.db");
        seed(&src);
        let export = extract_sessions(&src, Uuid::new_v4(), None).unwrap();
        // A non-session record (no raw_source_format.session) must be ignored.
        let mut recs = export.records.clone();
        let mut bogus = recs[0].clone();
        bogus.raw_source_format = Some(serde_json::json!({"store":"memory"}));
        recs.push(bogus);
        let dst = tmp.path().join("rebuilt.db");
        let n = rebuild_state_db(&dst, &recs, &export.schema).unwrap();
        assert_eq!(n, 2, "only real session records are rebuilt");
    }
}
