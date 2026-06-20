//! Phase 0 rebuild spike for `adapter-hermes`.
//!
//! Proves the load-bearing, no-precedent claim in the design: that a Hermes
//! `state.db` can be **decomposed into ALF-style records and rebuilt** such
//! that real Hermes code opens it. This mirrors exactly what the real adapter
//! will do (`session_extractor.rs` + `session_rebuilder.rs`), but standalone.
//!
//! Pipeline:
//!   1. DECOMPOSE  — open source `state.db` read-only; capture its own `CREATE`
//!      statements (skipping FTS5 auto-shadow tables + sqlite_sequence), and
//!      read every session + its messages into one JSON record per session.
//!      This is the `raw_source_format` boundary — write it to `records.json`.
//!   2. REBUILD    — create a fresh DB, REPLAY the captured DDL (so we are
//!      schema-version-agnostic — no hand-maintained schema), then INSERT
//!      sessions + messages from the JSON. Hermes's triggers repopulate both
//!      FTS tables automatically.
//!   3. COMPARE    — structural row-level diff (sqldiff isn't available) of
//!      sessions + messages, and an FTS query parity check.
//!
//! Usage: rebuild_spike <source.db> <rebuilt.db> <records.json>

use rusqlite::types::{Value, ValueRef};
use rusqlite::{Connection, OpenFlags};
use serde_json::{json, Map, Value as J};
use std::collections::BTreeMap;
use std::path::Path;
use std::process::exit;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() != 4 {
        eprintln!("usage: rebuild_spike <source.db> <rebuilt.db> <records.json>");
        exit(2);
    }
    let (src, dst, records_path) = (&args[1], &args[2], &args[3]);

    let report = match run(src, dst, records_path) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("SPIKE ERROR: {e}");
            exit(1);
        }
    };
    println!("{}", serde_json::to_string_pretty(&report).unwrap());
    let ok = report["ok"].as_bool().unwrap_or(false);
    exit(if ok { 0 } else { 1 });
}

fn run(src: &str, dst: &str, records_path: &str) -> Result<J, String> {
    // ---- 1. DECOMPOSE ---------------------------------------------------
    let s = Connection::open_with_flags(src, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|e| format!("open source: {e}"))?;

    let ddl = capture_ddl(&s)?;
    let schema_version: i64 = s
        .query_row("SELECT version FROM schema_version LIMIT 1", [], |r| r.get(0))
        .unwrap_or(-1);

    let sessions = read_rows(&s, "SELECT * FROM sessions ORDER BY started_at")?;
    let messages = read_rows(&s, "SELECT * FROM messages ORDER BY id")?;

    // Group messages under their session → one ALF record per session.
    let mut records: Vec<J> = Vec::new();
    for sess in &sessions {
        let sid = sess.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string();
        let msgs: Vec<J> = messages
            .iter()
            .filter(|m| m.get("session_id").and_then(|v| v.as_str()) == Some(&sid))
            .cloned()
            .map(J::Object)
            .collect();
        records.push(json!({ "session": sess, "messages": msgs }));
    }
    let records_doc = json!({
        "schema_version": schema_version,
        "ddl": ddl,
        "records": records,
    });
    std::fs::write(records_path, serde_json::to_vec_pretty(&records_doc).unwrap())
        .map_err(|e| format!("write records.json: {e}"))?;

    // ---- 2. REBUILD (from the JSON records only) ------------------------
    if Path::new(dst).exists() {
        let _ = std::fs::remove_file(dst);
        let _ = std::fs::remove_file(format!("{dst}-wal"));
        let _ = std::fs::remove_file(format!("{dst}-shm"));
    }
    let d = Connection::open(dst).map_err(|e| format!("open dst: {e}"))?;

    // Replay the source's own DDL, ordered so dependencies exist first:
    // real tables → FTS virtual tables → indexes → triggers.
    for stmt in order_ddl(&ddl) {
        d.execute_batch(&stmt)
            .map_err(|e| format!("replay DDL failed: {e}\n--- stmt ---\n{stmt}"))?;
    }
    if schema_version >= 0 {
        d.execute(
            "INSERT INTO schema_version (version) VALUES (?1)",
            [schema_version],
        )
        .map_err(|e| format!("insert schema_version: {e}"))?;
    }

    // Insert sessions, then messages (with explicit id → FTS rowid alignment).
    // FK enforcement is off by default (as in Hermes), so order is free.
    for rec in &records {
        let sess = rec.get("session").and_then(|v| v.as_object()).unwrap();
        insert_row(&d, "sessions", sess)?;
        if let Some(msgs) = rec.get("messages").and_then(|v| v.as_array()) {
            for m in msgs {
                insert_row(&d, "messages", m.as_object().unwrap())?;
            }
        }
    }

    // ---- 3. COMPARE -----------------------------------------------------
    let rebuilt_sessions = read_rows(&d, "SELECT * FROM sessions ORDER BY started_at")?;
    let rebuilt_messages = read_rows(&d, "SELECT * FROM messages ORDER BY id")?;

    let mut diffs: Vec<String> = Vec::new();
    compare("sessions", &sessions, &rebuilt_sessions, &mut diffs);
    compare("messages", &messages, &rebuilt_messages, &mut diffs);

    // FTS parity: every message's tokens must be findable in the rebuilt index.
    let fts_src = fts_hits(&s, "retry")?;
    let fts_dst = fts_hits(&d, "retry")?;
    if fts_src != fts_dst {
        diffs.push(format!(
            "messages_fts('retry') mismatch: source={fts_src:?} rebuilt={fts_dst:?}"
        ));
    }
    // Trigram needs a contiguous ≥3-char substring ("起草发" is in the seeded
    // CJK message); 2-char or non-contiguous queries match nothing by design.
    let trig_dst = fts_hits_table(&d, "messages_fts_trigram", "起草发")?;
    if trig_dst.is_empty() {
        diffs.push("messages_fts_trigram('起草发') returned no hits in rebuilt DB".into());
    }

    Ok(json!({
        "ok": diffs.is_empty(),
        "schema_version": schema_version,
        "ddl_statements_replayed": ddl.len(),
        "sessions": sessions.len(),
        "messages": messages.len(),
        "fts_keyword_hits": fts_dst.len(),
        "fts_trigram_hits": trig_dst.len(),
        "diffs": diffs,
    }))
}

/// Capture the source DB's own `CREATE` statements, dropping the entries that
/// SQLite/FTS5 recreate automatically (FTS shadow tables + sqlite_sequence).
fn capture_ddl(c: &Connection) -> Result<Vec<String>, String> {
    let mut stmt = c
        .prepare("SELECT name, sql FROM sqlite_master WHERE sql IS NOT NULL")
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
        .map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    for row in rows {
        let (name, sql) = row.map_err(|e| e.to_string())?;
        if name == "sqlite_sequence" || is_fts_shadow(&name) {
            continue;
        }
        out.push(sql);
    }
    Ok(out)
}

/// FTS5 shadow tables created implicitly by `CREATE VIRTUAL TABLE ... fts5`.
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

/// Order DDL so replay dependencies are satisfied.
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
            indexes.push(s.clone()); // CREATE [UNIQUE] INDEX
        }
    }
    [tables, virtuals, indexes, triggers].concat()
}

/// Read all rows of a query into `Vec<JSON object>` preserving SQLite types.
fn read_rows(c: &Connection, sql: &str) -> Result<Vec<Map<String, J>>, String> {
    let mut stmt = c.prepare(sql).map_err(|e| e.to_string())?;
    let cols: Vec<String> = stmt.column_names().iter().map(|s| s.to_string()).collect();
    let mut rows = stmt.query([]).map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    while let Some(row) = rows.next().map_err(|e| e.to_string())? {
        let mut obj = Map::new();
        for (i, name) in cols.iter().enumerate() {
            obj.insert(name.clone(), valueref_to_json(row.get_ref(i).map_err(|e| e.to_string())?));
        }
        out.push(obj);
    }
    Ok(out)
}

fn valueref_to_json(v: ValueRef<'_>) -> J {
    match v {
        ValueRef::Null => J::Null,
        ValueRef::Integer(i) => json!(i),
        ValueRef::Real(f) => json!(f),
        ValueRef::Text(t) => json!(String::from_utf8_lossy(t)),
        // No BLOBs occur in sessions/messages; tag them defensively if they do.
        ValueRef::Blob(b) => json!({ "$blob_len": b.len() }),
    }
}

/// Build and run an INSERT from a JSON row object, binding values back to
/// native SQLite types (column affinity resolves int-vs-real on REAL columns).
fn insert_row(c: &Connection, table: &str, row: &Map<String, J>) -> Result<(), String> {
    let cols: Vec<&String> = row.keys().collect();
    let placeholders: Vec<String> = (1..=cols.len()).map(|i| format!("?{i}")).collect();
    let sql = format!(
        "INSERT INTO {table} ({}) VALUES ({})",
        cols.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", "),
        placeholders.join(", ")
    );
    let values: Vec<Value> = cols.iter().map(|c| json_to_value(&row[*c])).collect();
    let params: Vec<&dyn rusqlite::ToSql> =
        values.iter().map(|v| v as &dyn rusqlite::ToSql).collect();
    c.execute(&sql, params.as_slice())
        .map_err(|e| format!("insert into {table}: {e}"))?;
    Ok(())
}

fn json_to_value(v: &J) -> Value {
    match v {
        J::Null => Value::Null,
        J::Bool(b) => Value::Integer(*b as i64),
        J::Number(n) => {
            if let Some(i) = n.as_i64() {
                Value::Integer(i)
            } else {
                Value::Real(n.as_f64().unwrap())
            }
        }
        J::String(s) => Value::Text(s.clone()),
        other => Value::Text(other.to_string()),
    }
}

/// Row-by-row structural compare; records first divergence per row.
fn compare(label: &str, a: &[Map<String, J>], b: &[Map<String, J>], diffs: &mut Vec<String>) {
    if a.len() != b.len() {
        diffs.push(format!("{label}: row count {} vs {}", a.len(), b.len()));
        return;
    }
    for (i, (ra, rb)) in a.iter().zip(b.iter()).enumerate() {
        let ma: BTreeMap<_, _> = ra.iter().collect();
        let mb: BTreeMap<_, _> = rb.iter().collect();
        if ma != mb {
            for (k, va) in &ma {
                if mb.get(k) != Some(va) {
                    diffs.push(format!(
                        "{label}[{i}].{k}: {:?} vs {:?}",
                        va,
                        mb.get(k)
                    ));
                }
            }
        }
    }
}

fn fts_hits(c: &Connection, term: &str) -> Result<Vec<i64>, String> {
    fts_hits_table(c, "messages_fts", term)
}

fn fts_hits_table(c: &Connection, table: &str, term: &str) -> Result<Vec<i64>, String> {
    let mut stmt = c
        .prepare(&format!("SELECT rowid FROM {table} WHERE {table} MATCH ?1 ORDER BY rowid"))
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([term], |r| r.get::<_, i64>(0))
        .map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r.map_err(|e| e.to_string())?);
    }
    Ok(out)
}
