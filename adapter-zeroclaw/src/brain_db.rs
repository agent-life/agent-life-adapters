//! ZeroClaw `brain.db` schema capture + per-agent slice restore.
//!
//! `brain.db` is a **shared** SQLite store partitioned by `agent_id`: one row
//! per agent in `agents`, `UNIQUE(agent_id, key)` on `memories`, and an FTS5
//! shadow table `memories_fts` maintained entirely by the `memories_ai/ad/au`
//! triggers. ALF captures one agent's slice on export and restores it on import
//! **without disturbing other agents' rows**.
//!
//! Mirrors the Hermes `session_extractor` / `session_rebuilder` DDL-sidecar
//! pattern (capture the source DDL as a raw-source sidecar, replay it on a
//! lazy-store bootstrap, let the runtime's own triggers repopulate FTS), but the
//! restore is **slice-scoped** — it never nukes the shared DB (capture plan D6).

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use rusqlite::types::Value as SqlValue;
use rusqlite::Connection;
use serde::{Deserialize, Serialize};

use alf_core::{MemoryRecord, RestoreMode};

/// Archive-relative sidecar name (written under `raw/zeroclaw/`). Consumed by
/// restore, never materialized into the workspace (like Hermes' schema sidecar).
pub const SCHEMA_SIDECAR: &str = ".alf-brain-db-schema.json";

/// The captured `brain.db` schema, replayed verbatim when a target install has
/// no `brain.db` yet (lazy-store bootstrap).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BrainDbSchema {
    /// `CREATE` statements from the source DB, minus FTS5 auto-shadow tables and
    /// `sqlite_sequence` (SQLite/FTS5 recreate those from the virtual table).
    pub ddl: Vec<String>,
    /// `schema_version` rows — replayed on a bootstrap so the ZeroClaw daemon
    /// does not re-migrate a hand-built DB. Empty when the table is absent.
    #[serde(default)]
    pub schema_version: Vec<SchemaVersionRow>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SchemaVersionRow {
    pub component: String,
    pub version: i64,
    pub applied_at: String,
}

/// One native `memories` row, reconstructed for restore. `id`/`content` come
/// from the ALF record; the rest from its `raw_source_format` stash (or the
/// semantic fields, for older archives). `embedding` is best-effort re-encoded.
#[derive(Debug, Clone)]
pub struct NativeRow {
    pub id: String,
    pub key: String,
    pub content: String,
    pub category: String,
    pub embedding: Option<Vec<u8>>,
    pub created_at: String,
    pub updated_at: String,
    pub session_id: Option<String>,
    pub namespace: String,
    pub importance: f64,
    pub superseded_by: Option<String>,
}

impl NativeRow {
    /// Reconstruct a native `memories` row from an ALF record. Returns `None` for
    /// records without a ZeroClaw stash (e.g. cross-runtime archives — those are
    /// not brain.db rows). Prefers the verbatim `raw_source_format` stash and
    /// falls back to the ALF-semantic fields for forward/backward compatibility.
    /// The embedding is re-encoded best-effort as packed little-endian f32
    /// (ZeroClaw's default); NULL when the record carries no embedding.
    pub fn from_record(rec: &MemoryRecord) -> Option<NativeRow> {
        let raw = rec.raw_source_format.as_ref();
        let get_str = |k: &str| {
            raw.and_then(|r| r.get(k))
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
        };
        let key = get_str("key")?; // no key ⇒ not a brain.db row
        let created_at =
            get_str("created_at").unwrap_or_else(|| rec.temporal.created_at.to_rfc3339());
        let updated_at = get_str("updated_at")
            .or_else(|| rec.temporal.updated_at.map(|t| t.to_rfc3339()))
            .unwrap_or_else(|| created_at.clone());
        let namespace = get_str("namespace")
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| rec.namespace.clone());
        let importance = raw
            .and_then(|r| r.get("importance"))
            .and_then(|v| v.as_f64())
            .or(rec.confidence)
            .unwrap_or(0.5);
        let superseded_by =
            get_str("superseded_by").or_else(|| rec.supersedes.map(|u| u.to_string()));
        let session_id = get_str("session_id").or_else(|| rec.source.session_id.clone());
        let embedding = rec.embeddings.first().map(|e| {
            e.vector
                .iter()
                .flat_map(|f| (*f as f32).to_le_bytes())
                .collect::<Vec<u8>>()
        });
        Some(NativeRow {
            id: rec.id.to_string(),
            key,
            content: rec.content.clone(),
            category: get_str("category")
                .or_else(|| rec.category.clone())
                .unwrap_or_else(|| "core".to_string()),
            embedding,
            created_at,
            updated_at,
            session_id,
            namespace,
            importance,
            superseded_by,
        })
    }
}

/// The ZeroClaw agent identity recorded in the archive (manifest provenance).
#[derive(Debug, Clone)]
pub struct ArchivedAgent {
    pub id: Option<String>,
    pub alias: String,
    pub created_at: Option<String>,
}

/// What a slice restore did — for the ImportReport + provenance warnings.
#[derive(Debug, Clone)]
pub struct RestoreOutcome {
    pub rows_written: usize,
    /// The `agents.id` the slice was written under (may differ from the archived
    /// id when the alias already existed under a different id).
    pub resolved_agent_id: String,
    /// Set when the target alias existed under an id other than the archived one.
    pub remapped_from: Option<String>,
    /// True when `brain.db` was created from the captured DDL (was lazily absent).
    pub bootstrapped: bool,
}

// ---------------------------------------------------------------------------
// WAL copy-read (D6)
// ---------------------------------------------------------------------------

/// A private, writable copy of `brain.db` + its `-wal`/`-shm` sidecars in a temp
/// dir. The daemon holds the live DB open in WAL mode; copying the trio lets us
/// read un-checkpointed rows without racing the daemon or writing the live
/// store. The `TempDir` must outlive the returned `Connection`.
pub struct ReadCopy {
    _dir: tempfile::TempDir,
    pub conn: Connection,
}

/// Copy `db_path` (+ WAL/SHM sidecars) to a temp dir and open the copy. Opened
/// read-write on the throwaway copy so a WAL-mode DB reads cleanly; the live
/// store is never touched.
pub fn open_readonly_copy(db_path: &Path) -> Result<ReadCopy> {
    let dir = tempfile::tempdir().context("creating brain.db copy-read tempdir")?;
    let copy = dir.path().join("brain.db");
    fs::copy(db_path, &copy).with_context(|| format!("copying {} for read", db_path.display()))?;
    for suffix in ["-wal", "-shm"] {
        let side = with_suffix(db_path, suffix);
        if side.is_file() {
            let _ = fs::copy(&side, with_suffix(&copy, suffix));
        }
    }
    // Read-write open on the throwaway copy: a WAL-mode DB may need to create a
    // `-shm` file, and reads see committed + WAL rows. The live DB is untouched.
    let conn = Connection::open(&copy)
        .with_context(|| format!("opening brain.db copy: {}", copy.display()))?;
    Ok(ReadCopy { _dir: dir, conn })
}

// ---------------------------------------------------------------------------
// Schema capture
// ---------------------------------------------------------------------------

/// Capture the source DB's own `CREATE` statements (minus `sqlite_sequence` and
/// FTS5 shadow tables, which the virtual table recreates) plus its
/// `schema_version` rows.
pub fn capture_schema(conn: &Connection) -> Result<BrainDbSchema> {
    let mut stmt = conn.prepare("SELECT name, sql FROM sqlite_master WHERE sql IS NOT NULL")?;
    let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
    let mut ddl = Vec::new();
    for row in rows {
        let (name, sql) = row?;
        if name == "sqlite_sequence" || is_fts_shadow(&name) {
            continue;
        }
        ddl.push(sql);
    }
    Ok(BrainDbSchema {
        ddl,
        schema_version: read_schema_version(conn).unwrap_or_default(),
    })
}

fn read_schema_version(conn: &Connection) -> Result<Vec<SchemaVersionRow>> {
    let exists: bool = conn
        .prepare("SELECT name FROM sqlite_master WHERE type='table' AND name='schema_version'")?
        .exists([])?;
    if !exists {
        return Ok(Vec::new());
    }
    let mut stmt = conn.prepare("SELECT component, version, applied_at FROM schema_version")?;
    let rows = stmt.query_map([], |r| {
        Ok(SchemaVersionRow {
            component: r.get(0)?,
            version: r.get(1)?,
            applied_at: r.get(2)?,
        })
    })?;
    Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
}

/// Read the `agents` table `(id, alias)` rows, oldest first (stable identity
/// continuity: the pre-existing agent sorts before later ones). Returns an empty
/// vec when the table is absent (lazy store).
pub fn read_agents(conn: &Connection) -> Result<Vec<(String, String)>> {
    let exists: bool = conn
        .prepare("SELECT name FROM sqlite_master WHERE type='table' AND name='agents'")?
        .exists([])?;
    if !exists {
        return Ok(Vec::new());
    }
    let mut stmt =
        conn.prepare("SELECT id, alias FROM agents ORDER BY created_at ASC, alias ASC")?;
    let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
    Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
}

/// Copy-read the `agents` table from a live `brain.db` (discovery path). Returns
/// an empty vec when the file is absent.
pub fn read_agents_from_path(db_path: &Path) -> Result<Vec<(String, String)>> {
    if !db_path.is_file() {
        return Ok(Vec::new());
    }
    let copy = open_readonly_copy(db_path)?;
    read_agents(&copy.conn)
}

/// FTS5 auto-created shadow tables (`<fts>_data/_idx/_docsize/_config/_content`).
/// The virtual table recreates these, so replaying them would be redundant.
fn is_fts_shadow(name: &str) -> bool {
    ["_data", "_idx", "_docsize", "_config", "_content"]
        .iter()
        .any(|s| name.strip_suffix(s).is_some_and(|base| !base.is_empty()))
}

/// Order DDL so replay dependencies are satisfied: real tables → fts5 virtual
/// tables → indexes → triggers.
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

// ---------------------------------------------------------------------------
// Slice restore
// ---------------------------------------------------------------------------

/// Restore one agent's slice into `db_path`, leaving every other agent's rows
/// byte-identical.
///
/// - Bootstraps `brain.db` from the captured DDL when it is lazily absent.
/// - Resolves the target `agents.id`: reuse the row already bound to the alias
///   (remap recorded when it differs from the archive), else `INSERT` a row
///   carrying the archived id for provenance.
/// - `Total`: single transaction — delete the agent's slice, insert the archive
///   rows. `Merge`: per-agent upsert on `(agent_id, key)`.
///
/// FTS is maintained by the `memories_ai/ad/au` triggers — `memories_fts` is
/// never written directly.
pub fn restore_agent_slice(
    db_path: &Path,
    schema: &BrainDbSchema,
    archived: &ArchivedAgent,
    rows: &[NativeRow],
    mode: RestoreMode,
    now: &str,
) -> Result<RestoreOutcome> {
    let bootstrapped = !db_path.is_file();
    if bootstrapped {
        bootstrap(db_path, schema, now)?;
    }
    let mut conn = Connection::open(db_path)
        .with_context(|| format!("opening brain.db for restore: {}", db_path.display()))?;
    // Foreign keys off: we insert `memories` referencing an `agents` row we
    // guarantee below, but leave other agents' FK graph untouched regardless.
    conn.pragma_update(None, "foreign_keys", false).ok();

    let (resolved_agent_id, remapped_from) = resolve_target_agent(&conn, archived, now)?;

    let written = match mode {
        RestoreMode::Total => restore_total(&mut conn, &resolved_agent_id, rows)?,
        RestoreMode::Merge => restore_merge(&mut conn, &resolved_agent_id, rows)?,
    };

    Ok(RestoreOutcome {
        rows_written: written,
        resolved_agent_id,
        remapped_from,
        bootstrapped,
    })
}

/// Create a fresh `brain.db` from the captured schema (lazy-store bootstrap).
fn bootstrap(db_path: &Path, schema: &BrainDbSchema, now: &str) -> Result<()> {
    if let Some(parent) = db_path.parent() {
        fs::create_dir_all(parent)?;
    }
    let conn = Connection::open(db_path)
        .with_context(|| format!("creating brain.db: {}", db_path.display()))?;
    if schema.ddl.is_empty() {
        anyhow::bail!(
            "cannot bootstrap a missing brain.db: the archive carries no captured schema \
             (re-export with a schema-capable alf, or run the framework once to create the store)"
        );
    }
    for stmt in order_ddl(&schema.ddl) {
        conn.execute_batch(&stmt)
            .with_context(|| format!("replaying brain.db DDL failed:\n{stmt}"))?;
    }
    for sv in &schema.schema_version {
        let applied = if sv.applied_at.is_empty() {
            now
        } else {
            sv.applied_at.as_str()
        };
        conn.execute(
            "INSERT OR REPLACE INTO schema_version (component, version, applied_at) \
             VALUES (?1, ?2, ?3)",
            rusqlite::params![sv.component, sv.version, applied],
        )
        .ok();
    }
    Ok(())
}

/// Resolve the `agents.id` to write the slice under. Returns the resolved id and
/// `Some(existing_id)` when it was remapped away from the archived id.
fn resolve_target_agent(
    conn: &Connection,
    archived: &ArchivedAgent,
    now: &str,
) -> Result<(String, Option<String>)> {
    let existing: Option<String> = conn
        .query_row(
            "SELECT id FROM agents WHERE alias = ?1",
            [&archived.alias],
            |r| r.get(0),
        )
        .ok();

    if let Some(existing_id) = existing {
        let remapped = match &archived.id {
            Some(aid) if aid != &existing_id => Some(aid.clone()),
            _ => None,
        };
        return Ok((existing_id, remapped));
    }

    // Alias absent — create it, preferring the archived id for provenance.
    let created = archived.created_at.as_deref().unwrap_or(now);
    if let Some(aid) = &archived.id {
        let inserted = conn
            .execute(
                "INSERT OR IGNORE INTO agents (id, alias, created_at) VALUES (?1, ?2, ?3)",
                rusqlite::params![aid, archived.alias, created],
            )
            .context("inserting target agents row")?;
        if inserted == 1 {
            return Ok((aid.clone(), None));
        }
        // The archived id already belongs to a DIFFERENT alias (e.g. the agent
        // was renamed with a stable id). Writing the slice under that id would
        // overwrite another agent's rows — the "other agents byte-identical"
        // invariant. Allocate a fresh id for this alias instead and record the
        // remap so import surfaces it; never touch the colliding agent's slice.
        let fresh = uuid::Uuid::new_v4().to_string();
        conn.execute(
            "INSERT INTO agents (id, alias, created_at) VALUES (?1, ?2, ?3)",
            rusqlite::params![fresh, archived.alias, created],
        )
        .context("inserting target agents row (remapped after id collision)")?;
        return Ok((fresh, Some(aid.clone())));
    }

    // No archived id at all — mint a fresh one for the alias.
    let fresh = uuid::Uuid::new_v4().to_string();
    conn.execute(
        "INSERT OR IGNORE INTO agents (id, alias, created_at) VALUES (?1, ?2, ?3)",
        rusqlite::params![fresh, archived.alias, created],
    )
    .context("inserting target agents row")?;
    Ok((fresh, None))
}

fn restore_total(conn: &mut Connection, agent_id: &str, rows: &[NativeRow]) -> Result<usize> {
    let tx = conn.transaction().context("begin total-restore tx")?;
    tx.execute("DELETE FROM memories WHERE agent_id = ?1", [agent_id])
        .context("clearing agent slice")?;
    let mut n = 0usize;
    for row in rows {
        insert_row(&tx, agent_id, row)?;
        n += 1;
    }
    tx.commit().context("commit total-restore tx")?;
    Ok(n)
}

fn restore_merge(conn: &mut Connection, agent_id: &str, rows: &[NativeRow]) -> Result<usize> {
    let tx = conn.transaction().context("begin merge-restore tx")?;
    let mut n = 0usize;
    for row in rows {
        upsert_row(&tx, agent_id, row)?;
        n += 1;
    }
    tx.commit().context("commit merge-restore tx")?;
    Ok(n)
}

const INSERT_COLS: &str = "id, key, content, category, embedding, created_at, updated_at, \
                           session_id, namespace, importance, superseded_by, agent_id";

fn row_params<'a>(agent_id: &'a str, row: &'a NativeRow) -> [SqlValue; 12] {
    [
        SqlValue::Text(row.id.clone()),
        SqlValue::Text(row.key.clone()),
        SqlValue::Text(row.content.clone()),
        SqlValue::Text(row.category.clone()),
        match &row.embedding {
            Some(b) => SqlValue::Blob(b.clone()),
            None => SqlValue::Null,
        },
        SqlValue::Text(row.created_at.clone()),
        SqlValue::Text(row.updated_at.clone()),
        match &row.session_id {
            Some(s) => SqlValue::Text(s.clone()),
            None => SqlValue::Null,
        },
        SqlValue::Text(row.namespace.clone()),
        SqlValue::Real(row.importance),
        match &row.superseded_by {
            Some(s) => SqlValue::Text(s.clone()),
            None => SqlValue::Null,
        },
        SqlValue::Text(agent_id.to_string()),
    ]
}

fn insert_row(conn: &Connection, agent_id: &str, row: &NativeRow) -> Result<()> {
    let params = row_params(agent_id, row);
    let refs: Vec<&dyn rusqlite::ToSql> =
        params.iter().map(|v| v as &dyn rusqlite::ToSql).collect();
    conn.execute(
        &format!(
            "INSERT INTO memories ({INSERT_COLS}) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)"
        ),
        refs.as_slice(),
    )
    .with_context(|| format!("inserting memory row key={}", row.key))?;
    Ok(())
}

/// Upsert on the `(agent_id, key)` unique constraint: archive wins on the
/// payload columns, but the local row's `id`/rowid (and thus FTS rowid) stay
/// stable so the update trigger re-syncs FTS cleanly.
fn upsert_row(conn: &Connection, agent_id: &str, row: &NativeRow) -> Result<()> {
    let params = row_params(agent_id, row);
    let refs: Vec<&dyn rusqlite::ToSql> =
        params.iter().map(|v| v as &dyn rusqlite::ToSql).collect();
    conn.execute(
        &format!(
            "INSERT INTO memories ({INSERT_COLS}) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12) \
             ON CONFLICT(agent_id, key) DO UPDATE SET \
               content=excluded.content, category=excluded.category, embedding=excluded.embedding, \
               created_at=excluded.created_at, updated_at=excluded.updated_at, \
               session_id=excluded.session_id, namespace=excluded.namespace, \
               importance=excluded.importance, superseded_by=excluded.superseded_by"
        ),
        refs.as_slice(),
    )
    .with_context(|| format!("upserting memory row key={}", row.key))?;
    Ok(())
}

fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut s = path.as_os_str().to_owned();
    s.push(suffix);
    PathBuf::from(s)
}

// ---------------------------------------------------------------------------
// Test fixtures (shared with sqlite_extractor / import tests)
// ---------------------------------------------------------------------------

/// The real captured schema, split into individual statements with the explicit
/// FTS5 shadow-table CREATEs dropped (the virtual table recreates them). This is
/// the canonical real-DDL builder input for the whole crate's unit tests
/// (capture plan §7 — no synthetic `memory.db` schema).
#[cfg(test)]
pub(crate) fn committed_ddl() -> Vec<String> {
    let raw = include_str!("../testkit/captured/brain.db.schema.sql");
    split_sql(raw)
        .into_iter()
        .filter(|s| {
            let u = s.to_uppercase();
            !(u.contains("MEMORIES_FTS_DATA")
                || u.contains("MEMORIES_FTS_IDX")
                || u.contains("MEMORIES_FTS_DOCSIZE")
                || u.contains("MEMORIES_FTS_CONFIG"))
        })
        .collect()
}

/// Build a `brain.db` at `dir/brain.db` from the committed real DDL, with an
/// `agents` row per `(id, alias)`. `dir` is created if absent.
#[cfg(test)]
pub(crate) fn real_schema_db(dir: &Path, agents: &[(&str, &str)]) -> PathBuf {
    fs::create_dir_all(dir).unwrap();
    let db = dir.join("brain.db");
    let conn = Connection::open(&db).unwrap();
    for stmt in order_ddl(&committed_ddl()) {
        conn.execute_batch(&stmt).unwrap();
    }
    conn.execute(
        "INSERT OR REPLACE INTO schema_version (component, version, applied_at) \
         VALUES ('memories', 3, '2026-01-01T00:00:00Z')",
        [],
    )
    .unwrap();
    for (id, alias) in agents {
        conn.execute(
            "INSERT INTO agents (id, alias, created_at) VALUES (?1, ?2, '2026-01-01T00:00:00Z')",
            rusqlite::params![id, alias],
        )
        .unwrap();
    }
    db
}

/// Split a `.schema` dump into statements: `;` ends a statement, except inside a
/// `CREATE TRIGGER … BEGIN … END;` body (whose inner `;`s are ignored).
#[cfg(test)]
fn split_sql(raw: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut in_trigger = false;
    for line in raw.lines() {
        cur.push_str(line);
        cur.push('\n');
        let u = line.trim().to_uppercase();
        if u.starts_with("CREATE TRIGGER") {
            in_trigger = true;
        }
        if in_trigger {
            if u == "END;" {
                in_trigger = false;
                out.push(std::mem::take(&mut cur));
            }
            continue;
        }
        if line.trim_end().ends_with(';') {
            out.push(std::mem::take(&mut cur));
        }
    }
    if !cur.trim().is_empty() {
        out.push(cur);
    }
    out.into_iter()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn row(id: &str, key: &str, content: &str) -> NativeRow {
        NativeRow {
            id: id.into(),
            key: key.into(),
            content: content.into(),
            category: "core".into(),
            embedding: None,
            created_at: "2026-01-15T10:00:00Z".into(),
            updated_at: "2026-01-15T10:00:00Z".into(),
            session_id: None,
            namespace: "default".into(),
            importance: 0.5,
            superseded_by: None,
        }
    }

    fn schema_from(db: &Path) -> BrainDbSchema {
        let conn = Connection::open(db).unwrap();
        capture_schema(&conn).unwrap()
    }

    fn count(db: &Path, agent_id: &str) -> i64 {
        let conn = Connection::open(db).unwrap();
        conn.query_row(
            "SELECT COUNT(*) FROM memories WHERE agent_id = ?1",
            [agent_id],
            |r| r.get(0),
        )
        .unwrap()
    }

    #[test]
    fn capture_drops_shadow_tables_keeps_virtual_and_triggers() {
        let dir = tempfile::tempdir().unwrap();
        let db = real_schema_db(dir.path(), &[]);
        let schema = schema_from(&db);
        let joined = schema.ddl.join("\n").to_uppercase();
        assert!(joined.contains("CREATE VIRTUAL TABLE MEMORIES_FTS"));
        assert!(joined.contains("CREATE TRIGGER MEMORIES_AI"));
        assert!(!joined.contains("MEMORIES_FTS_DATA"));
        assert!(!joined.contains("MEMORIES_FTS_IDX"));
        assert_eq!(
            schema
                .schema_version
                .iter()
                .find(|s| s.component == "memories")
                .map(|s| s.version),
            Some(3)
        );
    }

    #[test]
    fn total_restore_replaces_target_leaves_other_agents() {
        let dir = tempfile::tempdir().unwrap();
        let a = "aaaaaaaa-0000-0000-0000-000000000001";
        let b = "bbbbbbbb-0000-0000-0000-000000000002";
        let db = real_schema_db(dir.path(), &[(a, "agent_a"), (b, "agent_b")]);
        let schema = schema_from(&db);

        // Seed both agents directly (triggers maintain FTS).
        let conn = Connection::open(&db).unwrap();
        for (aid, key) in [(a, "a_old"), (b, "b_keep")] {
            insert_row(&conn, aid, &row(&uuid_for(aid, key), key, "seed")).unwrap();
        }
        drop(conn);

        // Total-restore agent_a with a fresh single row.
        let new_rows = [row(
            "11111111-0000-0000-0000-000000000009",
            "a_new",
            "restored",
        )];
        let out = restore_agent_slice(
            &db,
            &schema,
            &ArchivedAgent {
                id: Some(a.into()),
                alias: "agent_a".into(),
                created_at: None,
            },
            &new_rows,
            RestoreMode::Total,
            "2026-06-01T00:00:00Z",
        )
        .unwrap();
        assert_eq!(out.resolved_agent_id, a);
        assert_eq!(out.rows_written, 1);
        assert!(out.remapped_from.is_none());

        // agent_a now equals the archive (only a_new); agent_b untouched.
        assert_eq!(count(&db, a), 1);
        let conn = Connection::open(&db).unwrap();
        let has_new: bool = conn
            .prepare("SELECT 1 FROM memories WHERE agent_id=?1 AND key='a_new'")
            .unwrap()
            .exists([a])
            .unwrap();
        assert!(has_new);
        assert_eq!(count(&db, b), 1);
        // FTS is consistent — a MATCH on the restored content finds it.
        let hits: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM memories_fts WHERE memories_fts MATCH 'restored'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(hits, 1);
    }

    #[test]
    fn merge_keeps_local_only_rows_and_updates_conflicts() {
        let dir = tempfile::tempdir().unwrap();
        let a = "aaaaaaaa-0000-0000-0000-000000000001";
        let db = real_schema_db(dir.path(), &[(a, "agent_a")]);
        let schema = schema_from(&db);

        let conn = Connection::open(&db).unwrap();
        insert_row(&conn, a, &row(&uuid_for(a, "shared"), "shared", "old")).unwrap();
        insert_row(
            &conn,
            a,
            &row(&uuid_for(a, "local_only"), "local_only", "keep"),
        )
        .unwrap();
        drop(conn);

        let archive = [row("22222222-0000-0000-0000-000000000009", "shared", "new")];
        restore_agent_slice(
            &db,
            &schema,
            &ArchivedAgent {
                id: Some(a.into()),
                alias: "agent_a".into(),
                created_at: None,
            },
            &archive,
            RestoreMode::Merge,
            "2026-06-01T00:00:00Z",
        )
        .unwrap();

        assert_eq!(count(&db, a), 2, "local-only row survives merge");
        let conn = Connection::open(&db).unwrap();
        let shared: String = conn
            .query_row(
                "SELECT content FROM memories WHERE agent_id=?1 AND key='shared'",
                [a],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(shared, "new", "archive wins on conflict");
    }

    #[test]
    fn remap_when_alias_exists_under_different_id() {
        let dir = tempfile::tempdir().unwrap();
        let live = "cccccccc-0000-0000-0000-000000000003";
        let db = real_schema_db(dir.path(), &[(live, "agent_a")]);
        let schema = schema_from(&db);
        let out = restore_agent_slice(
            &db,
            &schema,
            &ArchivedAgent {
                id: Some("dddddddd-0000-0000-0000-000000000004".into()),
                alias: "agent_a".into(),
                created_at: None,
            },
            &[row("33333333-0000-0000-0000-000000000009", "k", "v")],
            RestoreMode::Total,
            "2026-06-01T00:00:00Z",
        )
        .unwrap();
        assert_eq!(out.resolved_agent_id, live, "targets the live id");
        assert_eq!(
            out.remapped_from.as_deref(),
            Some("dddddddd-0000-0000-0000-000000000004")
        );
    }

    #[test]
    fn archived_id_colliding_under_different_alias_never_wipes_it() {
        // The agent was renamed (id stable): live 'agent_c' holds id U. An older
        // archive stamped alias='agent_b' with the SAME id U must NOT delete or
        // overwrite agent_c's slice — it remaps to a fresh id.
        let dir = tempfile::tempdir().unwrap();
        let u = "cccccccc-0000-0000-0000-00000000000c";
        let db = real_schema_db(dir.path(), &[(u, "agent_c")]);
        let schema = schema_from(&db);
        let conn = Connection::open(&db).unwrap();
        insert_row(&conn, u, &row(&uuid_for(u, "keep"), "keep", "c-data")).unwrap();
        drop(conn);

        let out = restore_agent_slice(
            &db,
            &schema,
            &ArchivedAgent {
                id: Some(u.into()),
                alias: "agent_b".into(),
                created_at: None,
            },
            &[row(
                "aaaaaaaa-0000-0000-0000-000000000009",
                "b_key",
                "b-data",
            )],
            RestoreMode::Total,
            "2026-06-01T00:00:00Z",
        )
        .unwrap();

        assert_ne!(out.resolved_agent_id, u, "must NOT reuse the colliding id");
        assert_eq!(out.remapped_from.as_deref(), Some(u), "remap recorded");
        // agent_c's slice is completely intact.
        assert_eq!(count(&db, u), 1);
        let conn = Connection::open(&db).unwrap();
        let kept: String = conn
            .query_row("SELECT content FROM memories WHERE agent_id=?1", [u], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(kept, "c-data");
    }

    #[test]
    fn bootstrap_creates_brain_db_from_ddl() {
        let dir = tempfile::tempdir().unwrap();
        // Capture a schema from a throwaway DB, then restore into a missing path.
        let seed = real_schema_db(&dir.path().join("seed"), &[]);
        let schema = schema_from(&seed);

        let target = dir.path().join("data/memory/brain.db");
        assert!(!target.is_file());
        let out = restore_agent_slice(
            &target,
            &schema,
            &ArchivedAgent {
                id: Some("eeeeeeee-0000-0000-0000-000000000005".into()),
                alias: "solo".into(),
                created_at: None,
            },
            &[row("44444444-0000-0000-0000-000000000009", "k", "v")],
            RestoreMode::Total,
            "2026-06-01T00:00:00Z",
        )
        .unwrap();
        assert!(out.bootstrapped);
        assert!(target.is_file());
        assert_eq!(count(&target, "eeeeeeee-0000-0000-0000-000000000005"), 1);
    }

    fn uuid_for(agent: &str, key: &str) -> String {
        uuid::Uuid::new_v5(
            &uuid::Uuid::NAMESPACE_URL,
            format!("{agent}:{key}").as_bytes(),
        )
        .to_string()
    }
}
