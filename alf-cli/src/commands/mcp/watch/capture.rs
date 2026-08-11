//! SQLite capture primitives — **reserved for v2 DB-row extraction** (design §10).
//!
//! Quiescence (the "never sync torn bytes" guard) is enforced entirely by the
//! pure scheduler's timing gate ([`engine`](super::engine)): a source is deferred
//! until its change events stop for the debounce window, with **no SQLite
//! exemption** — a live raw `.db` waits like any file (WP-M3 review A2), because
//! the v1 generic capture is a plain single-file read and exempting it would ship
//! torn/uncheckpointed bytes.
//!
//! These helpers ([`is_sqlite`], [`sqlite_snapshot`]) are the consistent-snapshot
//! primitives the **v2** row-extraction path will use (reading rows out of a
//! `VACUUM INTO` copy rather than preserving bytes). They are not on the v1 raw
//! path, so they are `#[allow(dead_code)]` until v2 wires them in.

use std::fs;
use std::path::Path;

use anyhow::{Context, Result};

/// SQLite database file header (bytes 0..16 of every SQLite file).
#[allow(dead_code)]
pub const SQLITE_MAGIC: &[u8; 16] = b"SQLite format 3\0";

/// True if `path` is a SQLite database (by its 16-byte header). A read error or
/// short file is treated as "not SQLite".
#[allow(dead_code)]
pub fn is_sqlite(path: &Path) -> bool {
    let Ok(mut file) = fs::File::open(path) else {
        return false;
    };
    use std::io::Read;
    let mut header = [0u8; 16];
    match file.read_exact(&mut header) {
        Ok(()) => &header == SQLITE_MAGIC,
        Err(_) => false,
    }
}

/// A transactionally-consistent snapshot of a SQLite database at `src`, written
/// to `dst`, using `VACUUM INTO`. Safe to run against a live, actively-written
/// WAL-mode database — it takes a read transaction and never blocks writers for
/// long. `dst` must not already exist (a `VACUUM INTO` precondition).
///
/// **Not used in the v1 raw path, by design.** `VACUUM INTO` produces a
/// *defragmented* database that is byte-for-byte different from the original, so
/// it cannot back the raw-fidelity model (the adapter round-trip tests diff the
/// captured bytes against the source). In v1 a raw SQLite tracked file is
/// captured byte-preserving (the db+wal+shm file-copy the zeroclaw adapter
/// already uses). This primitive is retained + tested for the **v2 DB-row
/// extraction** path (design §10: "DB-row→record extraction is v2"), which reads
/// rows out of a consistent snapshot rather than preserving bytes.
#[allow(dead_code)]
pub fn sqlite_snapshot(src: &Path, dst: &Path) -> Result<()> {
    use rusqlite::Connection;
    // Open read-only so we never mutate the live store.
    let conn = Connection::open_with_flags(
        src,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_URI,
    )
    .with_context(|| format!("opening SQLite database {}", src.display()))?;
    // VACUUM INTO binds the destination path as a parameter.
    conn.execute("VACUUM INTO ?1", [dst.to_string_lossy().as_ref()])
        .with_context(|| {
            format!(
                "VACUUM INTO snapshot of {} -> {}",
                src.display(),
                dst.display()
            )
        })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::TempDir;

    #[test]
    fn is_sqlite_detects_header() {
        let tmp = TempDir::new().unwrap();
        let db = tmp.path().join("x.db");
        let mut f = fs::File::create(&db).unwrap();
        f.write_all(SQLITE_MAGIC).unwrap();
        f.write_all(b"...rest of page...").unwrap();
        assert!(is_sqlite(&db));

        let txt = tmp.path().join("y.md");
        fs::write(&txt, "# not a database\n").unwrap();
        assert!(!is_sqlite(&txt));

        let missing = tmp.path().join("nope");
        assert!(!is_sqlite(&missing));
    }

    #[test]
    fn sqlite_snapshot_round_trips_rows() {
        let tmp = TempDir::new().unwrap();
        let src = tmp.path().join("live.db");
        {
            let conn = rusqlite::Connection::open(&src).unwrap();
            // WAL mode — the realistic live scenario.
            conn.pragma_update(None, "journal_mode", "WAL").unwrap();
            conn.execute("CREATE TABLE m (id INTEGER, body TEXT)", [])
                .unwrap();
            conn.execute("INSERT INTO m VALUES (1, 'hello')", [])
                .unwrap();
            // Leave the connection (and WAL) open to mimic a live DB.
            let dst = tmp.path().join("snap.db");
            sqlite_snapshot(&src, &dst).unwrap();

            let snap = rusqlite::Connection::open(&dst).unwrap();
            let body: String = snap
                .query_row("SELECT body FROM m WHERE id = 1", [], |r| r.get(0))
                .unwrap();
            assert_eq!(body, "hello");
            assert!(is_sqlite(&dst));
        }
    }
}
