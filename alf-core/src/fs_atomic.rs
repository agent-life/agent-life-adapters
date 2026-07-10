//! Atomic file writes (WP-H.1) — std-only, Lambda-ARM64-safe.
//!
//! A plain `fs::write` can be torn by a crash mid-write, leaving a
//! half-written target that poisons the next reader. [`write_atomic`] writes
//! to a sibling temp file, fsyncs, then renames over the target — the target
//! is always either the old bytes or the new bytes, never a mixture.

use std::fs;
use std::io;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime};

/// Per-process counter so concurrent writers in one process never collide on
/// the same temp name.
static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Temps older than this are considered abandoned by a crashed writer and are
/// swept on the next successful write (WP-H.3). Generous enough that a live
/// concurrent writer's fresh temp is never touched.
const STALE_TEMP_AGE: Duration = Duration::from_secs(24 * 60 * 60);

/// Write `bytes` to `path` atomically.
///
/// Writes a sibling temp file `{name}.tmp.{pid}.{counter}`, `sync_all`s it,
/// then renames it over `path`. On rename failure the temp is removed so a
/// cross-device or permission error never leaves a stray sibling. After a
/// successful rename, stale temps (same target, older than 24 h) left behind
/// by crashed writers are swept best-effort.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("write_atomic: {} has no file name", path.display()),
            )
        })?
        .to_string();
    let tmp = path.with_file_name(format!(
        "{name}.tmp.{}.{}",
        std::process::id(),
        TEMP_COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    {
        let mut file = fs::File::create(&tmp)?;
        io::Write::write_all(&mut file, bytes)?;
        file.sync_all()?;
    }
    if let Err(e) = fs::rename(&tmp, path) {
        let _ = fs::remove_file(&tmp);
        return Err(e);
    }
    if let Some(dir) = path.parent() {
        cleanup_stale_temps(dir, &name, STALE_TEMP_AGE);
    }
    Ok(())
}

/// Best-effort sweep of abandoned temp files for `target_name` in `dir`
/// (WP-H.3). Removes ONLY siblings named exactly `{target_name}.tmp.{digits}.
/// {digits}` whose mtime is older than `older_than`; anything else — other
/// files, other targets' temps, a live writer's fresh temp — survives. All
/// errors are ignored (a failed sweep must never fail the write that
/// triggered it).
fn cleanup_stale_temps(dir: &Path, target_name: &str, older_than: Duration) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let prefix = format!("{target_name}.tmp.");
    let now = SystemTime::now();
    for entry in entries.flatten() {
        let file_name = entry.file_name();
        let Some(name) = file_name.to_str() else {
            continue;
        };
        let Some(rest) = name.strip_prefix(prefix.as_str()) else {
            continue;
        };
        if !is_pid_counter_suffix(rest) {
            continue;
        }
        let stale = entry
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|modified| now.duration_since(modified).ok())
            .is_some_and(|age| age > older_than);
        if stale {
            let _ = fs::remove_file(entry.path());
        }
    }
}

/// True iff `rest` is `{digits}.{digits}` — the `{pid}.{counter}` tail our own
/// temp names carry.
fn is_pid_counter_suffix(rest: &str) -> bool {
    let mut parts = rest.split('.');
    matches!(
        (parts.next(), parts.next(), parts.next()),
        (Some(pid), Some(counter), None)
            if !pid.is_empty()
                && pid.bytes().all(|b| b.is_ascii_digit())
                && !counter.is_empty()
                && counter.bytes().all(|b| b.is_ascii_digit())
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn write_atomic_replaces_and_leaves_no_temp() {
        let dir = TempDir::new().unwrap();
        let target = dir.path().join("data.json");
        fs::write(&target, b"old contents").unwrap();

        write_atomic(&target, b"new contents").unwrap();

        assert_eq!(fs::read(&target).unwrap(), b"new contents");
        let siblings: Vec<String> = fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(siblings, vec!["data.json"], "a temp sibling leaked");
    }

    #[test]
    fn write_atomic_creates_a_new_file() {
        let dir = TempDir::new().unwrap();
        let target = dir.path().join("fresh.txt");
        write_atomic(&target, b"hello").unwrap();
        assert_eq!(fs::read(&target).unwrap(), b"hello");
    }

    #[test]
    fn stale_matching_temps_are_cleaned_on_next_write() {
        let dir = TempDir::new().unwrap();
        let target = dir.path().join("data.json");
        // An abandoned temp from a crashed writer, aged past the 24 h gate.
        let stale = dir.path().join("data.json.tmp.99999.7");
        fs::write(&stale, b"torn").unwrap();
        let old = SystemTime::now() - Duration::from_secs(25 * 60 * 60);
        fs::File::options()
            .write(true)
            .open(&stale)
            .unwrap()
            .set_modified(old)
            .unwrap();

        write_atomic(&target, b"good").unwrap();

        assert!(!stale.exists(), "stale matching temp must be swept");
        assert_eq!(fs::read(&target).unwrap(), b"good");
    }

    #[test]
    fn non_matching_siblings_survive() {
        let dir = TempDir::new().unwrap();
        let target = dir.path().join("data.json");
        let old = SystemTime::now() - Duration::from_secs(25 * 60 * 60);
        // Same age as a stale temp, but none of these match
        // `data.json.tmp.{digits}.{digits}` exactly.
        let survivors = [
            "other.txt.tmp.1.2",       // different target
            "data.json.tmp.abc.1",     // non-digit pid
            "data.json.tmp.1",         // missing counter
            "data.json.tmp.1.2.3",     // extra component
            "data.json.tmp.1.2.extra", // extra component (non-digit)
        ];
        for name in survivors {
            let p = dir.path().join(name);
            fs::write(&p, b"x").unwrap();
            fs::File::options()
                .write(true)
                .open(&p)
                .unwrap()
                .set_modified(old)
                .unwrap();
        }

        write_atomic(&target, b"good").unwrap();

        for name in survivors {
            assert!(
                dir.path().join(name).exists(),
                "non-matching sibling {name} was removed"
            );
        }
        assert_eq!(fs::read(&target).unwrap(), b"good");
    }

    #[test]
    fn fresh_temp_of_a_live_writer_survives_the_age_gate() {
        let dir = TempDir::new().unwrap();
        let target = dir.path().join("data.json");
        // A matching temp with a *current* mtime — a concurrent writer mid-write.
        let fresh = dir.path().join("data.json.tmp.4242.0");
        fs::write(&fresh, b"in flight").unwrap();

        write_atomic(&target, b"good").unwrap();

        assert!(
            fresh.exists(),
            "a fresh matching temp (live writer) must survive the 24 h gate"
        );
    }

    #[test]
    fn rename_failure_removes_the_temp() {
        // Renaming a file over an existing directory fails (EISDIR); the temp
        // written next to it must be cleaned up.
        let dir = TempDir::new().unwrap();
        let target = dir.path().join("data.json");
        fs::create_dir(&target).unwrap();
        assert!(write_atomic(&target, b"x").is_err());
        let leftover = fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .any(|e| e.file_name().to_string_lossy().contains(".tmp."));
        assert!(!leftover, "temp file leaked after a failed rename");
    }
}
