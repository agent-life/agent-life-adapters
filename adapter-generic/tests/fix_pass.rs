//! Tests for the WP-M1 review fix pass: the security cluster (S1–S4) and the
//! robustness/reporting holes (R2, R3, C1, C2). Glob/duration/version fixes are
//! unit-tested in `src/map.rs`; these exercise the export/import surface.

use std::fs;
use std::path::Path;
use std::sync::OnceLock;

use adapter_generic::GenericAdapter;
use alf_core::{Adapter, AlfReader, ImportOptions, MemoryRecord};
use tempfile::TempDir;
use uuid::Uuid;

/// Point ALF_HOME + HOME at a throwaway dir for the whole test process, so the
/// vault read/write path and the external-roots policy file never touch the
/// developer's real `~/.alf`.
fn isolate_home() {
    static HOME: OnceLock<TempDir> = OnceLock::new();
    HOME.get_or_init(|| {
        let dir = TempDir::new().unwrap();
        std::env::set_var("ALF_HOME", dir.path());
        std::env::set_var("HOME", dir.path());
        dir
    });
}

/// Serializes tests that read-modify-write the shared `$HOME/.alf/external-roots`
/// policy file (`add_allowed_root` is not designed for concurrent same-process
/// writers; parallel test threads would lose each other's blessed roots).
fn external_roots_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

fn write(ws: &Path, rel: &str, content: &str) {
    let p = ws.join(rel);
    if let Some(parent) = p.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(p, content).unwrap();
}

/// A workspace with a single `knowledge/**/*.md` semantic source.
fn knowledge_ws(tmp: &TempDir) -> std::path::PathBuf {
    let ws = tmp.path().join("ws");
    fs::create_dir_all(&ws).unwrap();
    write(
        &ws,
        ".alf-map.json",
        r#"{"version":1,"memory_sources":[
            {"id":"kb","glob":"knowledge/**/*.md","memory_type":"semantic",
             "namespace":"curated","chunking":"per_file"}]}"#,
    );
    ws
}

fn export(ws: &Path, out: &Path) -> alf_core::ExportReport {
    GenericAdapter.export(ws, out).expect("export failed")
}

fn archive_names(alf: &Path) -> Vec<String> {
    let reader = AlfReader::new(fs::File::open(alf).unwrap()).unwrap();
    reader.file_names()
}

fn memory(alf: &Path) -> Vec<MemoryRecord> {
    let mut reader = AlfReader::new(fs::File::open(alf).unwrap()).unwrap();
    reader.read_all_memory().unwrap()
}

// ---------------------------------------------------------------------------
// S1 — identity_file path traversal
// ---------------------------------------------------------------------------

#[test]
fn s1_absolute_and_escaping_identity_file_rejected_at_export() {
    isolate_home();
    let tmp = TempDir::new().unwrap();
    let ws = tmp.path().join("ws");
    fs::create_dir_all(&ws).unwrap();
    // A secret one level above the workspace.
    fs::write(tmp.path().join("secret.txt"), "SENTINEL-TOKEN").unwrap();

    for bad in ["/etc/hostname", "../secret.txt"] {
        write(
            &ws,
            ".alf-map.json",
            &format!(r#"{{"version":1,"identity_file":"{bad}","memory_sources":[]}}"#),
        );
        let out = tmp.path().join("out.alf");
        let err = GenericAdapter
            .export(&ws, &out)
            .expect_err(&format!("identity_file `{bad}` must be rejected"));
        let _ = err;
        // Nothing leaked: no archive, no sentinel bytes anywhere in an archive.
        assert!(
            !out.exists(),
            "a rejected export must not produce an archive"
        );
    }
}

#[test]
#[cfg(unix)]
fn s1_symlinked_identity_file_rejected() {
    isolate_home();
    let tmp = TempDir::new().unwrap();
    let ws = tmp.path().join("ws");
    fs::create_dir_all(&ws).unwrap();
    let secret = tmp.path().join("id_rsa");
    fs::write(&secret, "PRIVATE-KEY").unwrap();
    std::os::unix::fs::symlink(&secret, ws.join("IDENTITY.md")).unwrap();
    write(
        &ws,
        ".alf-map.json",
        r#"{"version":1,"identity_file":"IDENTITY.md","memory_sources":[]}"#,
    );
    let out = tmp.path().join("out.alf");
    // The symlink resolves outside the workspace → export errors (S1 canonicalize
    // guard), and the private key never enters an archive.
    assert!(GenericAdapter.export(&ws, &out).is_err());
    assert!(!out.exists());
}

// ---------------------------------------------------------------------------
// S2 — export follows in-workspace symlinks
// ---------------------------------------------------------------------------

#[test]
#[cfg(unix)]
fn s2_symlinked_source_file_not_read_or_packed() {
    isolate_home();
    let tmp = TempDir::new().unwrap();
    let ws = knowledge_ws(&tmp);
    let secret = tmp.path().join("id_rsa");
    fs::write(&secret, "PRIVATE-KEY-SENTINEL").unwrap();
    // A symlinked markdown file that a `knowledge/**` glob would otherwise ingest.
    fs::create_dir_all(ws.join("knowledge")).unwrap();
    std::os::unix::fs::symlink(&secret, ws.join("knowledge/leak.md")).unwrap();
    // Plus a real file so the export still has content.
    write(&ws, "knowledge/real.md", "# Real\n\nsafe note");

    let out = tmp.path().join("out.alf");
    export(&ws, &out);

    // The symlink target's bytes appear nowhere: not as a record, not in raw.
    assert!(
        !memory(&out)
            .iter()
            .any(|r| r.content.contains("PRIVATE-KEY")),
        "symlink target leaked into a memory record"
    );
    let mut leaked = false;
    let mut reader = AlfReader::new(fs::File::open(&out).unwrap()).unwrap();
    for name in reader.file_names() {
        if name.starts_with("raw/generic/") {
            if let Ok(bytes) = reader.read_raw_entry_capped(&name, alf_core::MAX_RAW_ENTRY_BYTES) {
                if bytes.windows(9).any(|w| w == b"PRIVATE-K") {
                    leaked = true;
                }
            }
        }
    }
    assert!(!leaked, "symlink target leaked into the raw tree");
    // The real file still made it.
    assert!(memory(&out).iter().any(|r| r.content.contains("safe note")));
}

// ---------------------------------------------------------------------------
// S3 — broad glob ingests ALF control files
// ---------------------------------------------------------------------------

#[test]
fn s3_control_files_never_become_records_under_star_star() {
    isolate_home();
    let tmp = TempDir::new().unwrap();
    let ws = tmp.path().join("ws");
    fs::create_dir_all(&ws).unwrap();
    // A pre-pinned agent id + ignore + sync log so all control files are present.
    write(&ws, ".alf-agent-id", "f47ac10b-58cc-4372-a567-0e02b2c3d479");
    write(&ws, ".alf-sync-log.md", "- 2026-01-01: removed x\n");
    write(&ws, ".alfignore", "# nothing\n");
    write(&ws, ".alf-include.lock", "");
    write(
        &ws,
        ".alf-map.json",
        r#"{"version":1,"memory_sources":[
            {"id":"all","glob":"**","memory_type":"semantic",
             "namespace":"curated","chunking":"per_file"}]}"#,
    );
    write(&ws, "note.md", "# Note\n\nreal content");

    let out = tmp.path().join("out.alf");
    export(&ws, &out);
    let origins: Vec<String> = memory(&out)
        .into_iter()
        .filter_map(|r| r.source.origin_file)
        .collect();

    for control in [
        ".alf-map.json",
        ".alf-include.json",
        ".alf-include.lock",
        ".alf-sync-log.md",
        ".alf-agent-id",
        ".alfignore",
    ] {
        assert!(
            !origins.contains(&control.to_string()),
            "control file {control} became a memory record"
        );
    }
    // The real markdown still became a record.
    assert!(origins.contains(&"note.md".to_string()));
}

// ---------------------------------------------------------------------------
// S4 — import must not silently rebind a different agent id
// ---------------------------------------------------------------------------

#[test]
fn s4_import_fails_closed_on_agent_id_drift() {
    isolate_home();
    let tmp = TempDir::new().unwrap();
    // Build an archive for agent A.
    let src = knowledge_ws(&tmp);
    write(&src, "knowledge/a.md", "# A\n\nagent A memory");
    let alf = tmp.path().join("a.alf");
    export(&src, &alf);
    let archive_agent = AlfReader::new(fs::File::open(&alf).unwrap())
        .unwrap()
        .manifest()
        .agent
        .id;

    // A destination workspace already pinned to a DIFFERENT agent B.
    let dest = tmp.path().join("dest");
    fs::create_dir_all(&dest).unwrap();
    let other = Uuid::parse_str("00000000-0000-4000-8000-0000000000b0").unwrap();
    assert_ne!(other, archive_agent);
    fs::write(dest.join(".alf-agent-id"), other.to_string()).unwrap();
    fs::write(dest.join("keep.md"), "B's own file").unwrap();

    let err = GenericAdapter
        .import_with_options(&alf, &dest, ImportOptions::default())
        .expect_err("importing A's archive into B's workspace must fail closed");
    assert!(format!("{err:#}").contains("drift"));
    // Fail closed: B's pin and files untouched, A's tree not overlaid.
    assert_eq!(
        fs::read_to_string(dest.join(".alf-agent-id"))
            .unwrap()
            .trim(),
        other.to_string()
    );
    assert_eq!(
        fs::read_to_string(dest.join("keep.md")).unwrap(),
        "B's own file"
    );
    assert!(!dest.join("knowledge/a.md").exists());
}

// ---------------------------------------------------------------------------
// R2 — a non-UTF-8 matched file must not abort the export
// ---------------------------------------------------------------------------

#[test]
fn r2_non_utf8_source_is_warned_and_skipped_not_fatal() {
    isolate_home();
    let tmp = TempDir::new().unwrap();
    let ws = knowledge_ws(&tmp);
    write(&ws, "knowledge/good.md", "# Good\n\nreadable");
    // A binary "markdown" caught by knowledge/**/*.md.
    fs::create_dir_all(ws.join("knowledge")).unwrap();
    fs::write(ws.join("knowledge/blob.md"), [0xff, 0xfe, 0x00, 0x01]).unwrap();

    let out = tmp.path().join("out.alf");
    let report = export(&ws, &out);

    assert!(
        report
            .warnings
            .iter()
            .any(|w| w.contains("not valid UTF-8")),
        "expected a non-UTF-8 warning: {:?}",
        report.warnings
    );
    // The good file produced a record; the blob did not.
    let origins: Vec<String> = memory(&out)
        .into_iter()
        .filter_map(|r| r.source.origin_file)
        .collect();
    assert!(origins.contains(&"knowledge/good.md".to_string()));
    assert!(!origins.contains(&"knowledge/blob.md".to_string()));
    // But the blob is still preserved raw (fidelity).
    assert!(archive_names(&out)
        .iter()
        .any(|n| n == "raw/generic/knowledge/blob.md"));
}

// ---------------------------------------------------------------------------
// R3 — export is atomic (a failed export doesn't clobber a prior good archive)
// ---------------------------------------------------------------------------

#[test]
fn r3_failed_export_leaves_prior_output_untouched_and_no_temp_leak() {
    isolate_home();
    let tmp = TempDir::new().unwrap();
    let ws = knowledge_ws(&tmp);
    write(&ws, "knowledge/a.md", "# A\n\nfirst");

    // A good backup already exists at the output path.
    let out = tmp.path().join("backup.alf");
    export(&ws, &out);
    let good_bytes = fs::read(&out).unwrap();
    assert!(!good_bytes.is_empty());

    // Now corrupt the map so the next export fails during validation.
    write(
        &ws,
        ".alf-map.json",
        r#"{"version":1,"memory_sources":[
            {"id":"bad","glob":"knowledge/**/*.md","memory_type":"nonsense",
             "namespace":"curated","chunking":"per_file"}]}"#,
    );
    assert!(GenericAdapter.export(&ws, &out).is_err());

    // The prior good archive is byte-identical, and no `.tmp` sibling leaked.
    assert_eq!(fs::read(&out).unwrap(), good_bytes, "backup was clobbered");
    let leaked_temp = fs::read_dir(tmp.path())
        .unwrap()
        .filter_map(|e| e.ok())
        .any(|e| e.file_name().to_string_lossy().contains(".tmp."));
    assert!(!leaked_temp, "a temp file leaked");
}

// ---------------------------------------------------------------------------
// C1 — `.alfignore` honored + counted
// ---------------------------------------------------------------------------

#[test]
fn c1_alfignore_excludes_matched_files_and_is_counted() {
    isolate_home();
    let tmp = TempDir::new().unwrap();
    let ws = knowledge_ws(&tmp);
    write(&ws, "knowledge/keep.md", "# Keep\n\nkeep me");
    write(&ws, "knowledge/drop.md", "# Drop\n\ndrop me");
    write(&ws, ".alfignore", "knowledge/drop.md\n");

    let out = tmp.path().join("out.alf");
    let report = export(&ws, &out);

    assert_eq!(report.excluded_by_alfignore, 1);
    let origins: Vec<String> = memory(&out)
        .into_iter()
        .filter_map(|r| r.source.origin_file)
        .collect();
    assert!(origins.contains(&"knowledge/keep.md".to_string()));
    assert!(!origins.contains(&"knowledge/drop.md".to_string()));
    // And it's not carried raw either.
    assert!(!archive_names(&out)
        .iter()
        .any(|n| n == "raw/generic/knowledge/drop.md"));
}

// ---------------------------------------------------------------------------
// C2 — externals packed; malformed include-list warns (never silently drops)
// ---------------------------------------------------------------------------

#[test]
fn c2_external_tracked_file_is_packed_under_external() {
    isolate_home();
    let _roots_guard = external_roots_lock();
    let tmp = TempDir::new().unwrap();
    let ws = knowledge_ws(&tmp);
    write(&ws, "knowledge/a.md", "# A\n\nnote");

    // An external file outside the workspace, under a blessed root.
    let ext_root = tmp.path().join("shared");
    fs::create_dir_all(&ext_root).unwrap();
    let ext_file = ext_root.join("AGENTS.md");
    fs::write(&ext_file, "shared project doc").unwrap();
    let canon = ext_file.canonicalize().unwrap();
    alf_core::include::add_allowed_root(&ext_root).unwrap();

    let mut list = alf_core::include::IncludeList::default();
    let sanitized = alf_core::include::sanitized_external_name(&canon);
    list.add_external(&sanitized, canon.to_str().unwrap());
    list.save(&ws).unwrap();

    let out = tmp.path().join("out.alf");
    export(&ws, &out);
    assert!(
        archive_names(&out)
            .iter()
            .any(|n| n.starts_with("raw/generic/external/")),
        "external file was not packed"
    );
}

// ---------------------------------------------------------------------------
// WP-G — SQLite extraction hard-fails the export; consistent WAL capture
// ---------------------------------------------------------------------------

/// A workspace whose only source is a `sqlite_rows` map over `brain.db`.
fn sqlite_ws(tmp: &TempDir) -> std::path::PathBuf {
    let ws = tmp.path().join("ws");
    fs::create_dir_all(&ws).unwrap();
    write(
        &ws,
        ".alf-map.json",
        r#"{"version":1,"memory_sources":[
            {"id":"brain","glob":"brain.db","memory_type":"semantic",
             "namespace":"curated","chunking":"sqlite_rows",
             "sqlite":{"table":"memories","id_column":"id",
                       "content_column":"content","timestamp_column":"updated_at"}}]}"#,
    );
    ws
}

#[test]
fn sqlite_schema_error_hard_fails_the_export_with_marker() {
    isolate_home();
    let tmp = TempDir::new().unwrap();
    let ws = tmp.path().join("ws");
    fs::create_dir_all(&ws).unwrap();
    // The map names a content_column that does not exist → schema drift.
    write(
        &ws,
        ".alf-map.json",
        r#"{"version":1,"memory_sources":[
            {"id":"brain","glob":"brain.db","memory_type":"semantic",
             "namespace":"curated","chunking":"sqlite_rows",
             "sqlite":{"table":"memories","id_column":"id",
                       "content_column":"no_such_column"}}]}"#,
    );
    let conn = rusqlite::Connection::open(ws.join("brain.db")).unwrap();
    conn.execute_batch(
        "CREATE TABLE memories (id TEXT PRIMARY KEY, content TEXT);
         INSERT INTO memories VALUES ('a', 'alpha');",
    )
    .unwrap();
    drop(conn);

    let out = tmp.path().join("out.alf");
    let err = GenericAdapter
        .export(&ws, &out)
        .expect_err("a sqlite schema error must hard-fail the export (decision 4)");
    let msg = format!("{err:#}");
    assert!(
        msg.contains(adapter_generic::SQLITE_EXTRACTION_FAILED),
        "error must carry the classify() marker: {msg}"
    );
    assert!(
        msg.contains("`brain`"),
        "error must name the source id: {msg}"
    );
    // Hard fail means no archive was produced (silent zero-record degrade would
    // have mass-deleted the cloud history on the next delta).
    assert!(!out.exists(), "a failed export must not write an archive");
}

#[test]
fn wal_mode_db_packs_db_and_wal_sidecar() {
    isolate_home();
    let tmp = TempDir::new().unwrap();
    let ws = sqlite_ws(&tmp);
    let conn = rusqlite::Connection::open(ws.join("brain.db")).unwrap();
    conn.pragma_update(None, "journal_mode", "WAL").unwrap();
    conn.execute_batch(
        "CREATE TABLE memories (id TEXT PRIMARY KEY, content TEXT, updated_at TEXT);
         INSERT INTO memories VALUES ('a', 'wal-borne row', '2026-01-01T00:00:00Z');",
    )
    .unwrap();
    // The writing connection stays OPEN across the export: the row lives in the
    // live `-wal` sidecar, not the main db file.
    assert!(ws.join("brain.db-wal").exists());

    let out = tmp.path().join("out.alf");
    export(&ws, &out);
    drop(conn);

    let names = archive_names(&out);
    assert!(
        names.iter().any(|n| n == "raw/generic/brain.db"),
        "brain.db must be packed: {names:?}"
    );
    assert!(
        names.iter().any(|n| n == "raw/generic/brain.db-wal"),
        "the live -wal sidecar must be packed with the db (WP-G.3): {names:?}"
    );
    // The WAL-borne row was still extracted into a memory record.
    assert!(
        memory(&out).iter().any(|r| r.content == "wal-borne row"),
        "rows living only in the WAL must still extract"
    );
}

// ---------------------------------------------------------------------------
// WP-I.1 — size caps at export
// ---------------------------------------------------------------------------

#[test]
fn oversize_tracked_include_hard_fails_export() {
    isolate_home();
    let tmp = TempDir::new().unwrap();
    let ws = knowledge_ws(&tmp);
    write(&ws, "knowledge/a.md", "# A\n\nnote");
    // A tracked include over the per-entry cap (sparse → instant).
    let big = ws.join("big.bin");
    let f = fs::File::create(&big).unwrap();
    f.set_len(alf_core::MAX_RAW_ENTRY_BYTES + 1).unwrap();
    drop(f);
    let mut list = alf_core::include::IncludeList::default();
    list.add("big.bin");
    list.save(&ws).unwrap();

    let out = tmp.path().join("out.alf");
    let err = GenericAdapter
        .export(&ws, &out)
        .expect_err("an oversize tracked include must fail the export");
    let msg = format!("{err:#}");
    assert!(msg.contains("per-file cap"), "unexpected message: {msg}");
    assert!(!out.exists(), "a failed export must not write an archive");
}

#[test]
fn oversize_external_is_skipped_with_warning_at_export() {
    isolate_home();
    let _roots_guard = external_roots_lock();
    let tmp = TempDir::new().unwrap();
    let ws = knowledge_ws(&tmp);
    write(&ws, "knowledge/a.md", "# A\n\nnote");

    // An external file that was small (and valid) when added...
    let ext_root = tmp.path().join("shared-grow");
    fs::create_dir_all(&ext_root).unwrap();
    let ext_file = ext_root.join("GROWING.md");
    fs::write(&ext_file, "small at add time").unwrap();
    let canon = ext_file.canonicalize().unwrap();
    alf_core::include::add_allowed_root(&ext_root).unwrap();
    let mut list = alf_core::include::IncludeList::default();
    let sanitized = alf_core::include::sanitized_external_name(&canon);
    list.add_external(&sanitized, canon.to_str().unwrap());
    list.save(&ws).unwrap();

    // ...then grew past the cap before the next export.
    let f = fs::OpenOptions::new().write(true).open(&ext_file).unwrap();
    f.set_len(alf_core::MAX_RAW_ENTRY_BYTES + 1).unwrap();
    drop(f);

    let out = tmp.path().join("out.alf");
    let report = export(&ws, &out); // succeeds — external re-validation skips it
    assert!(
        report
            .warnings
            .iter()
            .any(|w| w.contains("rejected at export") && w.contains("per-file cap")),
        "expected an export-time skip warning: {:?}",
        report.warnings
    );
    assert!(
        !archive_names(&out)
            .iter()
            .any(|n| n.starts_with("raw/generic/external/")),
        "the oversize external must not be packed"
    );
}

#[test]
fn c2_malformed_include_list_warns_not_silent() {
    isolate_home();
    let tmp = TempDir::new().unwrap();
    let ws = knowledge_ws(&tmp);
    write(&ws, "knowledge/a.md", "# A\n\nnote");
    write(&ws, ".alf-include.json", "{ not json");

    let out = tmp.path().join("out.alf");
    let report = export(&ws, &out);
    assert!(
        report
            .warnings
            .iter()
            .any(|w| w.contains(".alf-include.json") && w.contains("could not be read")),
        "malformed include list must surface a warning: {:?}",
        report.warnings
    );
    // Export still succeeded and the real memory is present.
    assert!(memory(&out).iter().any(|r| r.content.contains("note")));
}

// ---------------------------------------------------------------------------
// BLK-1 — record ids are unique across every file a source's glob matches
// ---------------------------------------------------------------------------

#[test]
fn sqlite_multi_db_glob_mints_distinct_ids_per_file() {
    isolate_home();
    let tmp = TempDir::new().unwrap();
    let ws = tmp.path().join("ws");
    fs::create_dir_all(&ws).unwrap();
    // ONE sqlite source whose glob matches TWO databases with overlapping
    // autoincrement pks — the documented multi-file shape (map.rs: "each
    // matched file's rows become records").
    write(
        &ws,
        ".alf-map.json",
        r#"{"version":1,"memory_sources":[
            {"id":"brains","glob":"agents/*/brain.db","memory_type":"semantic",
             "namespace":"curated","chunking":"sqlite_rows",
             "sqlite":{"table":"memories","id_column":"id",
                       "content_column":"content"}}]}"#,
    );
    for (agent_dir, content) in [("a", "alpha"), ("b", "bravo")] {
        let db_path = ws.join("agents").join(agent_dir).join("brain.db");
        fs::create_dir_all(db_path.parent().unwrap()).unwrap();
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        conn.execute_batch(&format!(
            "CREATE TABLE memories (id INTEGER PRIMARY KEY, content TEXT);
             INSERT INTO memories VALUES (1, '{content}-1'), (2, '{content}-2');"
        ))
        .unwrap();
        drop(conn);
    }

    let out = tmp.path().join("out.alf");
    export(&ws, &out);
    let recs = memory(&out);
    assert_eq!(recs.len(), 4, "both databases' rows must become records");
    let ids: std::collections::HashSet<Uuid> = recs.iter().map(|r| r.id).collect();
    assert_eq!(
        ids.len(),
        recs.len(),
        "record ids must be unique across all glob-matched db files"
    );
    // Every row's content survived (no id-shadowed row loss downstream).
    for want in ["alpha-1", "alpha-2", "bravo-1", "bravo-2"] {
        assert!(
            recs.iter().any(|r| r.content == want),
            "row {want} lost to an id collision"
        );
    }
}
