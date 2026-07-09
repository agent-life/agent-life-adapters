//! Extract memory records from a SQLite source (`chunking: sqlite_rows`).
//!
//! Each row of the configured table becomes one [`MemoryRecord`]. The record id
//! is a deterministic v5 of the row's PRIMARY KEY (not its content), so an
//! in-place row `UPDATE` keeps the id and `alf_core::reconcile` emits exactly one
//! `Update` (pass P3), never a delete + create. The record carries **no**
//! `heading` slot in `raw_source_format`, so reconcile's markdown-heading pass
//! (P2) skips it and it can only pair by id — the correct behaviour for a
//! native-id store (a row whose content happens to start with `#` must never
//! heading-match a markdown section).
//!
//! The `.db` file itself is still preserved verbatim under `raw/generic/` by the
//! caller, so a same-runtime restore rewrites the database, not the rows.

use std::collections::HashMap;
use std::path::Path;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use rusqlite::types::Value;
use rusqlite::{Connection, OpenFlags};
use uuid::Uuid;

use alf_core::{ExtractionMethod, MemoryRecord, MemoryStatus, SourceProvenance, TemporalMetadata};

use crate::export::{build_tags, parse_memory_type, RUNTIME};
use crate::map::MemorySourceSpec;

/// Read `db_path`'s rows for `src` (a `sqlite_rows` source) into records.
///
/// Returns an empty vec when the configured table is absent (a lazy/empty store
/// must never fail the export). `rel_path` is the workspace-relative `.db` path
/// (the records' `origin_file`); `file_mtime` is the fallback timestamp.
pub fn extract_rows(
    db_path: &Path,
    rel_path: &str,
    src: &MemorySourceSpec,
    generic_ns: &Uuid,
    agent_id: Uuid,
    file_mtime: DateTime<Utc>,
) -> Result<Vec<MemoryRecord>> {
    let spec = src
        .sqlite
        .as_ref()
        .context("sqlite source missing its `sqlite` block")?;

    let conn = Connection::open_with_flags(db_path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .with_context(|| format!("opening sqlite db {}", db_path.display()))?;

    // Absent table → no records (do not fail the whole export on a lazy store).
    let table_exists: bool = conn
        .prepare("SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1")?
        .exists([spec.table.as_str()])?;
    if !table_exists {
        return Ok(Vec::new());
    }

    // Identifiers were validated identifier-safe at map-validate time; quote them
    // defensively. Order by the primary key for a stable extraction order.
    let ts_col = spec.timestamp_column.as_deref();
    let ts_sel = ts_col.map(|c| format!(", \"{c}\"")).unwrap_or_default();
    let sql = format!(
        "SELECT \"{id}\", \"{content}\"{ts} FROM \"{table}\" ORDER BY \"{id}\"",
        id = spec.id_column,
        content = spec.content_column,
        ts = ts_sel,
        table = spec.table,
    );

    let memory_type = parse_memory_type(&src.memory_type);
    let mut stmt = conn
        .prepare(&sql)
        .with_context(|| format!("preparing sqlite extraction query `{sql}`"))?;
    let mut query = stmt.query([])?;
    let mut records = Vec::new();
    while let Some(row) = query.next()? {
        let pk = value_to_string(row.get::<_, Value>(0)?);
        let content = value_to_string(row.get::<_, Value>(1)?);
        let ts_raw = match ts_col {
            Some(_) => match row.get::<_, Value>(2)? {
                Value::Null => None,
                v => Some(value_to_string(v)),
            },
            None => None,
        };

        // Content-INDEPENDENT id keyed on the row's primary key: an edit keeps it,
        // so reconcile P3 pairs it and emits exactly one Update.
        let id = Uuid::new_v5(
            generic_ns,
            format!("sqlite-row:{agent_id}:{}:{}:{pk}", src.id, spec.table).as_bytes(),
        );
        // A parseable timestamp column → created_at + observed_at; else the file
        // mtime (reconcile carries a matched row's created_at forward, so a mtime
        // that moves on every edit never surfaces as a spurious update).
        let parsed_ts = ts_raw
            .as_deref()
            .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
            .map(|dt| dt.with_timezone(&Utc));
        let (created_at, observed_at) = match parsed_ts {
            Some(t) => (t, Some(t)),
            None => (file_mtime, None),
        };

        let tags = build_tags(src, &content);
        // NB: no `heading` key — this is what routes the record past reconcile P2
        // to the id-match pass P3 (see the module docs).
        let raw_source_format = serde_json::json!({
            "sqlite_table": spec.table,
            "sqlite_id": pk,
        });
        records.push(MemoryRecord {
            id,
            agent_id,
            content,
            memory_type: memory_type.clone(),
            source: SourceProvenance {
                runtime: RUNTIME.to_string(),
                runtime_version: None,
                origin: Some("workspace".to_string()),
                origin_file: Some(rel_path.to_string()),
                extraction_method: Some(ExtractionMethod::AgentWritten),
                session_id: None,
                interaction_id: None,
                identity_version: None,
                extra: HashMap::new(),
            },
            temporal: TemporalMetadata {
                created_at,
                updated_at: Some(file_mtime),
                observed_at,
                valid_from: None,
                valid_until: None,
                last_accessed_at: None,
                access_count: None,
                extra: HashMap::new(),
            },
            status: MemoryStatus::Active,
            namespace: src.namespace.clone(),
            category: None,
            supersedes: None,
            confidence: None,
            entities: Vec::new(),
            tags,
            embeddings: Vec::new(),
            related_records: Vec::new(),
            raw_source_format: Some(raw_source_format),
            extra: HashMap::new(),
        });
    }
    Ok(records)
}

/// Canonical string for a sqlite cell: text as-is, integers/reals decimalised,
/// NULL as empty, a blob lossily decoded. Keeps the id preimage and content
/// stable across reads.
fn value_to_string(v: Value) -> String {
    match v {
        Value::Null => String::new(),
        Value::Integer(i) => i.to_string(),
        Value::Real(f) => f.to_string(),
        Value::Text(s) => s,
        Value::Blob(b) => String::from_utf8_lossy(&b).into_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alf_core::delta::compute_delta;
    use alf_core::reconcile::reconcile;
    use alf_core::DeltaOperation;
    use rusqlite::{params, Connection};

    const NS: Uuid = Uuid::from_bytes(*b"generic-alf-ns01");
    fn agent() -> Uuid {
        Uuid::from_u128(0x0123_4567_89ab_cdef)
    }

    fn source() -> MemorySourceSpec {
        serde_json::from_value(serde_json::json!({
            "id": "brain", "glob": "brain.db",
            "memory_type": "semantic", "namespace": "curated",
            "chunking": "sqlite_rows",
            "sqlite": {
                "table": "memories", "id_column": "id",
                "content_column": "content", "timestamp_column": "updated_at"
            }
        }))
        .unwrap()
    }

    fn build_db(path: &Path, rows: &[(&str, &str)]) {
        let conn = Connection::open(path).unwrap();
        conn.execute_batch(
            "CREATE TABLE memories (id TEXT PRIMARY KEY, content TEXT, updated_at TEXT);",
        )
        .unwrap();
        for (id, content) in rows {
            conn.execute(
                "INSERT INTO memories (id, content, updated_at) VALUES (?1, ?2, '2026-01-01T00:00:00Z')",
                params![id, content],
            )
            .unwrap();
        }
    }

    fn extract(path: &Path) -> Vec<MemoryRecord> {
        // A fixed mtime keeps the test deterministic; the timestamp column drives
        // created_at anyway.
        let mtime = DateTime::parse_from_rfc3339("2026-02-02T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        extract_rows(path, "brain.db", &source(), &NS, agent(), mtime).unwrap()
    }

    /// (creates, updates, deletes) for the current DB state vs `base`, run through
    /// reconcile exactly as `alf sync` does.
    fn delta_counts(base: &[MemoryRecord], path: &Path) -> (usize, usize, usize) {
        let curr = extract(path);
        let reconciled = reconcile(base, curr).records;
        let delta = compute_delta(base, &reconciled);
        let n = |op| delta.iter().filter(|e| e.operation == op).count();
        (
            n(DeltaOperation::Create),
            n(DeltaOperation::Update),
            n(DeltaOperation::Delete),
        )
    }

    #[test]
    fn rows_become_records_with_stable_pk_ids_and_no_heading() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("brain.db");
        build_db(&db, &[("a", "alpha"), ("b", "beta")]);
        let recs = extract(&db);
        assert_eq!(recs.len(), 2);
        assert_eq!(recs[0].content, "alpha");
        // pk-derived ids are stable across a re-extract (no content in the id).
        let again = extract(&db);
        assert_eq!(recs[0].id, again[0].id);
        assert_eq!(recs[1].id, again[1].id);
        // No `heading` slot → reconcile P2 skips these (they pair only by id, P3).
        assert!(recs[0]
            .raw_source_format
            .as_ref()
            .unwrap()
            .get("heading")
            .is_none());
        // created_at came from the timestamp column, not the file mtime.
        assert_eq!(
            recs[0].temporal.created_at.to_rfc3339(),
            "2026-01-01T00:00:00+00:00"
        );
    }

    #[test]
    fn touch_identical_reextract_is_zero_delta() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("brain.db");
        build_db(&db, &[("a", "alpha"), ("b", "beta")]);
        let base = extract(&db);
        assert_eq!(delta_counts(&base, &db), (0, 0, 0));
    }

    #[test]
    fn in_place_row_edit_is_exactly_one_update() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("brain.db");
        build_db(&db, &[("a", "alpha"), ("b", "beta"), ("c", "gamma")]);
        let base = extract(&db);
        // UPDATE keeps the primary key → the record keeps its id → P3 → one Update.
        Connection::open(&db)
            .unwrap()
            .execute("UPDATE memories SET content='BETA-edited' WHERE id='b'", [])
            .unwrap();
        assert_eq!(delta_counts(&base, &db), (0, 1, 0));
    }

    #[test]
    fn insert_row_is_exactly_one_create() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("brain.db");
        build_db(&db, &[("a", "alpha"), ("b", "beta")]);
        let base = extract(&db);
        Connection::open(&db)
            .unwrap()
            .execute(
                "INSERT INTO memories (id, content, updated_at) VALUES ('c', 'gamma', '2026-01-01T00:00:00Z')",
                [],
            )
            .unwrap();
        assert_eq!(delta_counts(&base, &db), (1, 0, 0));
    }

    #[test]
    fn delete_row_is_exactly_one_delete() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("brain.db");
        build_db(&db, &[("a", "alpha"), ("b", "beta"), ("c", "gamma")]);
        let base = extract(&db);
        Connection::open(&db)
            .unwrap()
            .execute("DELETE FROM memories WHERE id='b'", [])
            .unwrap();
        assert_eq!(delta_counts(&base, &db), (0, 0, 1));
    }

    #[test]
    fn absent_table_yields_no_records_not_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("brain.db");
        Connection::open(&db)
            .unwrap()
            .execute_batch("CREATE TABLE other (x TEXT);")
            .unwrap();
        assert!(extract(&db).is_empty());
    }

    #[test]
    fn integer_primary_keys_work() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("brain.db");
        let conn = Connection::open(&db).unwrap();
        conn.execute_batch(
            "CREATE TABLE memories (id INTEGER PRIMARY KEY, content TEXT, updated_at TEXT);",
        )
        .unwrap();
        conn.execute(
            "INSERT INTO memories (id, content, updated_at) VALUES (1, 'one', '2026-01-01T00:00:00Z')",
            [],
        )
        .unwrap();
        drop(conn);
        let base = extract(&db);
        assert_eq!(base.len(), 1);
        // Edit the integer-keyed row → still exactly one update.
        Connection::open(&db)
            .unwrap()
            .execute("UPDATE memories SET content='ONE' WHERE id=1", [])
            .unwrap();
        assert_eq!(delta_counts(&base, &db), (0, 1, 0));
    }
}
