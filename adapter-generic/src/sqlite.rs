//! Extract memory records from a SQLite source (`chunking: sqlite_rows`).
//!
//! Each row of the configured table becomes one [`MemoryRecord`]. The record id
//! is a deterministic v5 of (db file rel_path, source id, table, row PRIMARY
//! KEY) — not the row's content — so an in-place row `UPDATE` keeps the id and
//! `alf_core::reconcile` emits exactly one `Update` (pass P3), never a
//! delete + create; and rows with equal pks in *different* glob-matched `.db`
//! files never collide. The record carries **no**
//! `heading` slot in `raw_source_format`, so reconcile's markdown-heading pass
//! (P2) skips it and it can only pair by id — the correct behaviour for a
//! native-id store (a row whose content happens to start with `#` must never
//! heading-match a markdown section).
//!
//! The `.db` file itself is still preserved verbatim under `raw/generic/` by the
//! caller, so a same-runtime restore rewrites the database, not the rows.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use chrono::{DateTime, NaiveDateTime, Utc};
use rusqlite::types::Value;
use rusqlite::{Connection, OpenFlags};
use uuid::Uuid;

use alf_core::{ExtractionMethod, MemoryRecord, MemoryStatus, SourceProvenance, TemporalMetadata};

use crate::export::{build_tags, parse_memory_type, RUNTIME};
use crate::map::MemorySourceSpec;

/// Read `db_path`'s rows for `src` (a `sqlite_rows` source) into records, plus
/// human-readable warnings (at most one per source, counting rows whose
/// timestamp column was unparseable and fell back to the file mtime).
///
/// Returns empty vecs when the configured table is absent (a lazy/empty store
/// must never fail the export). Any *other* failure — locked database, corrupt
/// file, schema drift, NULL/duplicate primary keys — is a hard error: the
/// caller fails the whole export rather than degrade to zero records (which
/// would mass-delete cloud history). `rel_path` is the workspace-relative `.db`
/// path (the records' `origin_file`); `file_mtime` is the fallback timestamp.
pub fn extract_rows(
    db_path: &Path,
    rel_path: &str,
    src: &MemorySourceSpec,
    generic_ns: &Uuid,
    agent_id: Uuid,
    file_mtime: DateTime<Utc>,
) -> Result<(Vec<MemoryRecord>, Vec<String>)> {
    let spec = src
        .sqlite
        .as_ref()
        .context("sqlite source missing its `sqlite` block")?;

    let conn = Connection::open_with_flags(db_path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .with_context(|| format!("opening sqlite db {}", db_path.display()))?;
    // Wait out a short-lived writer lock instead of failing the export on the
    // first SQLITE_BUSY (§3.7.1: the reader waits up to 5 s for a busy db).
    conn.busy_timeout(Duration::from_secs(5))
        .context("setting sqlite busy_timeout")?;
    // Disable SQLite's double-quoted-string-literal misfeature: without this a
    // misconfigured column name (e.g. a typo'd content_column) silently
    // evaluates as a literal string for every row instead of erroring — the
    // exact silent-garbage failure the hard-fail contract (§3.7.1) forbids.
    conn.set_db_config(rusqlite::config::DbConfig::SQLITE_DBCONFIG_DQS_DML, false)
        .context("disabling sqlite double-quoted string literals")?;

    // Absent table → no records (do not fail the whole export on a lazy store).
    let table_exists: bool = conn
        .prepare("SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1")?
        .exists([spec.table.as_str()])?;
    if !table_exists {
        return Ok((Vec::new(), Vec::new()));
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
    let mut seen_pks: HashSet<String> = HashSet::new();
    let mut unparseable_timestamps: u64 = 0;
    while let Some(row) = query.next()? {
        let pk_value = row.get::<_, Value>(0)?;
        // NULL or duplicate primary keys are schema misconfigurations: the
        // pk-derived record ids would collide (silent row loss), so hard-fail
        // the extraction — the caller fails the export (decision 4).
        if matches!(pk_value, Value::Null) {
            bail!(
                "table \"{}\" has a row with NULL \"{}\": sqlite_rows requires a \
                 non-NULL primary key in id_column (point id_column at a NOT NULL \
                 unique key or fix the row)",
                spec.table,
                spec.id_column
            );
        }
        let pk = value_to_string(pk_value);
        if !seen_pks.insert(pk.clone()) {
            bail!(
                "table \"{}\" has a duplicate \"{}\" value {pk:?}: sqlite_rows \
                 requires unique id_column values (point id_column at a unique key \
                 or fix the rows)",
                spec.table,
                spec.id_column
            );
        }
        let content = value_to_string(row.get::<_, Value>(1)?);
        let ts_raw = match ts_col {
            Some(_) => match row.get::<_, Value>(2)? {
                Value::Null => None,
                v => Some(value_to_string(v)),
            },
            None => None,
        };

        // Content-INDEPENDENT id keyed on the row's primary key: an edit keeps it,
        // so reconcile P3 pairs it and emits exactly one Update. `rel_path` is a
        // discriminator because one source's glob may match several `.db` files
        // whose pks overlap — like text sources, the file path is identity-bearing
        // (so moving/renaming a database re-mints its rows' ids).
        let id = Uuid::new_v5(
            generic_ns,
            format!(
                "sqlite-row:{agent_id}:{rel_path}:{}:{}:{pk}",
                src.id, spec.table
            )
            .as_bytes(),
        );
        // A parseable timestamp column → created_at + observed_at; else the file
        // mtime (reconcile carries a matched row's created_at forward, so a mtime
        // that moves on every edit never surfaces as a spurious update).
        let parsed_ts = ts_raw.as_deref().and_then(parse_row_timestamp);
        if ts_raw.is_some() && parsed_ts.is_none() {
            unparseable_timestamps += 1;
        }
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
    let mut warnings = Vec::new();
    if unparseable_timestamps > 0 {
        warnings.push(format!(
            "sqlite source {rel_path}: {unparseable_timestamps} row(s) have an \
             unparseable \"{}\" value; fell back to the file mtime (accepted: \
             RFC3339, \"YYYY-MM-DD HH:MM:SS\" as UTC, or integer epoch seconds)",
            spec.timestamp_column.as_deref().unwrap_or("timestamp"),
        ));
    }
    Ok((records, warnings))
}

/// Parse a row timestamp (§3.7.1): RFC3339, SQLite's default
/// `YYYY-MM-DD HH:MM:SS[.fff]` (read as UTC), or integer epoch seconds —
/// anything else is `None` (the caller falls back to the file mtime and counts
/// it toward the per-source warning).
fn parse_row_timestamp(s: &str) -> Option<DateTime<Utc>> {
    let s = s.trim();
    if let Ok(dt) = DateTime::parse_from_rfc3339(s) {
        return Some(dt.with_timezone(&Utc));
    }
    if let Ok(naive) = NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S%.f") {
        return Some(naive.and_utc());
    }
    if let Ok(epoch) = s.parse::<i64>() {
        return DateTime::from_timestamp(epoch, 0);
    }
    None
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

    /// Fixed fallback mtime, keeping tests deterministic.
    fn fixed_mtime() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-02-02T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
    }

    fn extract_with_warnings(path: &Path) -> (Vec<MemoryRecord>, Vec<String>) {
        // A fixed mtime keeps the test deterministic; the timestamp column drives
        // created_at anyway.
        extract_rows(path, "brain.db", &source(), &NS, agent(), fixed_mtime()).unwrap()
    }

    fn extract(path: &Path) -> Vec<MemoryRecord> {
        extract_with_warnings(path).0
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
    fn null_pk_is_a_hard_error() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("brain.db");
        let conn = Connection::open(&db).unwrap();
        // No PRIMARY KEY constraint, so a NULL id row can exist.
        conn.execute_batch("CREATE TABLE memories (id TEXT, content TEXT, updated_at TEXT);")
            .unwrap();
        conn.execute(
            "INSERT INTO memories (id, content) VALUES (NULL, 'orphan')",
            [],
        )
        .unwrap();
        drop(conn);
        let err = extract_rows(&db, "brain.db", &source(), &NS, agent(), fixed_mtime())
            .expect_err("a NULL primary key must hard-fail the extraction");
        let msg = format!("{err:#}");
        assert!(msg.contains("NULL \"id\""), "unexpected message: {msg}");
        assert!(msg.contains("\"memories\""), "must name the table: {msg}");
    }

    #[test]
    fn duplicate_pk_is_a_hard_error_naming_the_pk() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("brain.db");
        let conn = Connection::open(&db).unwrap();
        conn.execute_batch(
            "CREATE TABLE memories (id TEXT, content TEXT, updated_at TEXT);
             INSERT INTO memories (id, content) VALUES ('dup', 'first');
             INSERT INTO memories (id, content) VALUES ('dup', 'second');",
        )
        .unwrap();
        drop(conn);
        let err = extract_rows(&db, "brain.db", &source(), &NS, agent(), fixed_mtime())
            .expect_err("a duplicate primary key must hard-fail the extraction");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("duplicate \"id\" value \"dup\""),
            "must name the offending pk: {msg}"
        );
    }

    #[test]
    fn busy_timeout_waits_out_a_short_writer_lock() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("brain.db");
        build_db(&db, &[("a", "alpha")]);
        // A writer holds an exclusive lock for 300 ms in another thread; the
        // reader's 5 s busy_timeout must wait it out instead of erroring.
        let writer = Connection::open(&db).unwrap();
        writer.execute_batch("BEGIN EXCLUSIVE;").unwrap();
        let handle = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(300));
            writer.execute_batch("COMMIT;").unwrap();
        });
        let recs = extract(&db);
        handle.join().unwrap();
        assert_eq!(recs.len(), 1);
        assert_eq!(recs[0].content, "alpha");
    }

    #[test]
    fn sqlite_default_datetime_format_parses_as_utc() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("brain.db");
        let conn = Connection::open(&db).unwrap();
        conn.execute_batch(
            "CREATE TABLE memories (id TEXT PRIMARY KEY, content TEXT, updated_at TEXT);
             INSERT INTO memories VALUES ('a', 'alpha', '2026-01-01 12:30:45');
             INSERT INTO memories VALUES ('b', 'beta', '2026-01-01 12:30:45.500');",
        )
        .unwrap();
        drop(conn);
        let (recs, warnings) = extract_with_warnings(&db);
        assert!(warnings.is_empty(), "no warning expected: {warnings:?}");
        assert_eq!(
            recs[0].temporal.created_at.to_rfc3339(),
            "2026-01-01T12:30:45+00:00",
            "SQLite's default datetime format must be read as UTC"
        );
        assert_eq!(
            recs[1].temporal.created_at.to_rfc3339(),
            "2026-01-01T12:30:45.500+00:00",
            "fractional seconds must parse too"
        );
        assert!(recs[0].temporal.observed_at.is_some());
    }

    #[test]
    fn epoch_seconds_timestamp_parses() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("brain.db");
        let conn = Connection::open(&db).unwrap();
        conn.execute_batch(
            "CREATE TABLE memories (id TEXT PRIMARY KEY, content TEXT, updated_at INTEGER);
             INSERT INTO memories VALUES ('a', 'alpha', 1767225600);",
        )
        .unwrap();
        drop(conn);
        let (recs, warnings) = extract_with_warnings(&db);
        assert!(warnings.is_empty(), "no warning expected: {warnings:?}");
        assert_eq!(
            recs[0].temporal.created_at.to_rfc3339(),
            "2026-01-01T00:00:00+00:00"
        );
    }

    #[test]
    fn unparseable_timestamps_warn_once_and_fall_back_to_mtime() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("brain.db");
        let conn = Connection::open(&db).unwrap();
        conn.execute_batch(
            "CREATE TABLE memories (id TEXT PRIMARY KEY, content TEXT, updated_at TEXT);
             INSERT INTO memories VALUES ('a', 'alpha', 'yesterday-ish');
             INSERT INTO memories VALUES ('b', 'beta', '01/02/2026');
             INSERT INTO memories VALUES ('c', 'gamma', '2026-01-01T00:00:00Z');",
        )
        .unwrap();
        drop(conn);
        let (recs, warnings) = extract_with_warnings(&db);
        // ONE warning for the source, counting both unparseable rows.
        assert_eq!(warnings.len(), 1, "exactly one warning: {warnings:?}");
        assert!(
            warnings[0].contains("2 row(s)"),
            "warning must count the rows: {}",
            warnings[0]
        );
        // Unparseable rows fell back to the file mtime, observed_at absent.
        assert_eq!(recs[0].temporal.created_at, fixed_mtime());
        assert!(recs[0].temporal.observed_at.is_none());
        // The parseable row still uses its own timestamp.
        assert_eq!(
            recs[2].temporal.created_at.to_rfc3339(),
            "2026-01-01T00:00:00+00:00"
        );
    }

    #[test]
    fn wal_mode_rows_are_extracted() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("brain.db");
        let conn = Connection::open(&db).unwrap();
        conn.pragma_update(None, "journal_mode", "WAL").unwrap();
        conn.execute_batch(
            "CREATE TABLE memories (id TEXT PRIMARY KEY, content TEXT, updated_at TEXT);
             INSERT INTO memories VALUES ('a', 'wal row', '2026-01-01T00:00:00Z');",
        )
        .unwrap();
        // The writing connection stays open: the row lives in the -wal file,
        // not yet checkpointed into the main db. The reader must still see it.
        assert!(db.with_file_name("brain.db-wal").exists());
        let recs = extract(&db);
        assert_eq!(recs.len(), 1);
        assert_eq!(recs[0].content, "wal row");
        drop(conn);
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

    // -- Id preimage discipline (injectivity twins) --------------------------
    //
    // Each twin varies exactly ONE discriminating dimension of the row-id
    // preimage and asserts the minted ids change; the no-op twin asserts
    // stability. A source's glob may match many `.db` files (map.rs: "each
    // matched file's rows become records"), so the matched file's rel_path IS
    // a dimension — omitting it collided same-pk rows across files (the
    // v1.1.0 pre-release BLK-1 bug).

    fn source_with(id: &str, table: &str) -> MemorySourceSpec {
        serde_json::from_value(serde_json::json!({
            "id": id, "glob": "**/brain.db",
            "memory_type": "semantic", "namespace": "curated",
            "chunking": "sqlite_rows",
            "sqlite": {
                "table": table, "id_column": "id",
                "content_column": "content", "timestamp_column": "updated_at"
            }
        }))
        .unwrap()
    }

    fn id_set(records: &[MemoryRecord]) -> HashSet<Uuid> {
        records.iter().map(|r| r.id).collect()
    }

    fn extract_ids(db: &Path, rel: &str, src: &MemorySourceSpec, agent_id: Uuid) -> HashSet<Uuid> {
        id_set(
            &extract_rows(db, rel, src, &NS, agent_id, fixed_mtime())
                .unwrap()
                .0,
        )
    }

    #[test]
    fn twin_identical_inputs_mint_identical_ids() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("brain.db");
        build_db(&db, &[("1", "alpha"), ("2", "beta")]);
        let a = extract_ids(&db, "agents/a/brain.db", &source(), agent());
        let b = extract_ids(&db, "agents/a/brain.db", &source(), agent());
        assert_eq!(a, b, "identical inputs must mint identical ids");
    }

    #[test]
    fn twin_same_pk_different_db_file_mints_distinct_ids() {
        // The BLK-1 regression: one source glob, two databases, overlapping
        // pks. The same physical db read under two rel_paths is the sharpest
        // form — every other preimage input is byte-identical.
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("brain.db");
        build_db(&db, &[("1", "alpha"), ("2", "beta")]);
        let a = extract_ids(&db, "agents/a/brain.db", &source(), agent());
        let b = extract_ids(&db, "agents/b/brain.db", &source(), agent());
        assert!(
            a.is_disjoint(&b),
            "same pk in two glob-matched db files must never mint the same record id"
        );
    }

    #[test]
    fn twin_different_source_id_mints_distinct_ids() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("brain.db");
        build_db(&db, &[("1", "alpha")]);
        let a = extract_ids(&db, "brain.db", &source_with("brain", "memories"), agent());
        let b = extract_ids(&db, "brain.db", &source_with("brain2", "memories"), agent());
        assert!(a.is_disjoint(&b), "source id must discriminate record ids");
    }

    #[test]
    fn twin_different_table_mints_distinct_ids() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("brain.db");
        let conn = Connection::open(&db).unwrap();
        conn.execute_batch(
            "CREATE TABLE memories (id TEXT PRIMARY KEY, content TEXT, updated_at TEXT);
             CREATE TABLE notes    (id TEXT PRIMARY KEY, content TEXT, updated_at TEXT);
             INSERT INTO memories VALUES ('1', 'alpha', '2026-01-01T00:00:00Z');
             INSERT INTO notes    VALUES ('1', 'alpha', '2026-01-01T00:00:00Z');",
        )
        .unwrap();
        drop(conn);
        let a = extract_ids(&db, "brain.db", &source_with("brain", "memories"), agent());
        let b = extract_ids(&db, "brain.db", &source_with("brain", "notes"), agent());
        assert!(a.is_disjoint(&b), "table must discriminate record ids");
    }

    #[test]
    fn twin_different_agent_mints_distinct_ids() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("brain.db");
        build_db(&db, &[("1", "alpha")]);
        let a = extract_ids(&db, "brain.db", &source(), agent());
        let b = extract_ids(&db, "brain.db", &source(), Uuid::from_u128(0xfeed));
        assert!(a.is_disjoint(&b), "agent id must discriminate record ids");
    }
}
