//! Restrictive file permissions for secrets under `~/.alf/`.

use std::io::Write;
use std::path::Path;

/// Write UTF-8 text so only the owner can read/write (Unix `0600`).
pub fn write_private(path: &Path, content: &str) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::fs::OpenOptions;
        use std::os::unix::fs::OpenOptionsExt;
        let mut f = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(path)?;
        f.write_all(content.as_bytes())
    }
    #[cfg(not(unix))]
    {
        std::fs::write(path, content)
    }
}

/// Create `path` exclusively (`O_EXCL`) with owner-only permissions (Unix
/// `0600`) and write `content`. Fails with [`std::io::ErrorKind::AlreadyExists`]
/// if the file exists. Use for a **generate-once** secret (a vault key) so two
/// concurrent first-writers converge on one file — the loser gets AlreadyExists
/// and re-reads the winner's key rather than clobbering it (WP-M3 review E1).
pub fn write_private_new(path: &Path, content: &str) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::fs::OpenOptions;
        use std::os::unix::fs::OpenOptionsExt;
        let mut f = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(path)?;
        f.write_all(content.as_bytes())
    }
    #[cfg(not(unix))]
    {
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)?;
        f.write_all(content.as_bytes())
    }
}

/// Write UTF-8 text atomically with owner-only permissions: a 0600 temp file
/// in the same directory, `sync_all`, then rename over `path`. A crash leaves
/// either the old file or the new one whole — never a truncated mix. Use for
/// secrets whose loss is unrecoverable (the vault document, migration stamps),
/// and for `config.toml` (WP-M5 review A1).
///
/// The temp file name is **process- and call-unique** (pid + an atomic counter),
/// so two concurrent processes writing the *same* target — e.g. two `alf`
/// invocations persisting discovery into one `~/.alf/config.toml`, or the M5
/// rediscovery tick racing a CLI writer — never collide on the temp or rename a
/// file the other already moved. This mirrors `configure::atomic_write`'s
/// suffix (WP-M3 review E1).
pub fn write_private_atomic(path: &Path, content: &str) -> std::io::Result<()> {
    write_private_atomic_bytes(path, content.as_bytes())
}

/// Byte-slice twin of [`write_private_atomic`] (0600 temp, `sync_all`, rename).
/// For binary payloads like the local delta base (`{id}-snapshot.alf`) — a
/// torn base would poison every later delta derivation (manual §4.5).
pub fn write_private_atomic_bytes(path: &Path, content: &[u8]) -> std::io::Result<()> {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);

    let file_name = path
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| std::io::Error::other("path has no file name"))?;
    let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
    let tmp = path.with_file_name(format!("{file_name}.tmp.{}.{unique}", std::process::id()));

    {
        #[cfg(unix)]
        let mut f = {
            use std::fs::OpenOptions;
            use std::os::unix::fs::OpenOptionsExt;
            OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .mode(0o600)
                .open(&tmp)?
        };
        #[cfg(not(unix))]
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(content)?;
        f.sync_all()?;
    }

    match std::fs::rename(&tmp, path) {
        Ok(()) => Ok(()),
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            Err(e)
        }
    }?;
    // A SIGKILL between temp-create and rename orphans a temp forever; sweep
    // stale ones (ours only, >24 h) after each successful write (manual §4.5).
    if let Some(dir) = path.parent() {
        cleanup_stale_temps(dir, file_name, std::time::Duration::from_secs(24 * 3600));
    }
    Ok(())
}

/// Best-effort removal of orphaned atomic-write temps for `target_name` in
/// `dir` older than `older_than`. Matches ONLY `{target_name}.tmp.{digits}.{digits}`
/// (our exact naming) — never anything else; all errors ignored. The age gate
/// keeps a live concurrent writer's fresh temp safe.
fn cleanup_stale_temps(dir: &Path, target_name: &str, older_than: std::time::Duration) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let prefix = format!("{target_name}.tmp.");
    for e in entries.flatten() {
        let name = e.file_name();
        let Some(name) = name.to_str() else { continue };
        let Some(rest) = name.strip_prefix(&prefix) else {
            continue;
        };
        let mut parts = rest.split('.');
        let numeric = matches!(
            (parts.next(), parts.next(), parts.next()),
            (Some(a), Some(b), None)
                if !a.is_empty() && !b.is_empty()
                    && a.bytes().all(|c| c.is_ascii_digit())
                    && b.bytes().all(|c| c.is_ascii_digit())
        );
        if !numeric {
            continue;
        }
        let stale = e
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|m| m.elapsed().ok())
            .map(|age| age > older_than)
            .unwrap_or(false);
        if stale {
            let _ = std::fs::remove_file(e.path());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn write_private_atomic_replaces_and_leaves_no_temp() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("credentials.json");

        write_private_atomic(&path, "{\"v\":1}").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "{\"v\":1}");

        write_private_atomic(&path, "{\"v\":2}").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "{\"v\":2}");

        // No .tmp sibling left behind.
        let leftovers: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().contains(".tmp"))
            .collect();
        assert!(leftovers.is_empty(), "temp file must not survive");
    }

    #[test]
    fn write_private_atomic_bytes_writes_binary_and_no_temp() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("base.alf");
        let payload = [0x50u8, 0x4b, 0x00, 0x03, 0xff]; // binary incl. a NUL
        write_private_atomic_bytes(&path, &payload).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), payload);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }
        let leftovers: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().contains(".tmp"))
            .collect();
        assert!(leftovers.is_empty(), "temp file must not survive");
    }

    #[test]
    fn stale_matching_temps_are_cleaned_on_next_write() {
        let dir = TempDir::new().unwrap();
        // An orphan from a dead process, made to look >24h old.
        let orphan = dir.path().join("credentials.json.tmp.999.7");
        std::fs::write(&orphan, "orphan").unwrap();
        let old = std::time::SystemTime::now() - std::time::Duration::from_secs(48 * 3600);
        std::fs::File::open(&orphan)
            .unwrap()
            .set_modified(old)
            .unwrap();

        write_private_atomic(&dir.path().join("credentials.json"), "{}").unwrap();
        assert!(!orphan.exists(), "the stale orphan is swept");
    }

    #[test]
    fn non_matching_siblings_survive_cleanup() {
        let dir = TempDir::new().unwrap();
        let keep = [
            "credentials.json.tmp.abc",   // non-numeric suffix
            "other.json.tmp.1.2",         // different target
            "credentials.json.bak",       // not a temp at all
            "credentials.json.tmp.1.2.3", // too many segments
        ];
        let old = std::time::SystemTime::now() - std::time::Duration::from_secs(48 * 3600);
        for name in keep {
            let p = dir.path().join(name);
            std::fs::write(&p, "x").unwrap();
            std::fs::File::open(&p).unwrap().set_modified(old).unwrap();
        }
        // A fresh temp of a live concurrent writer (matching name, young).
        let fresh = dir.path().join("credentials.json.tmp.888.1");
        std::fs::write(&fresh, "live").unwrap();

        write_private_atomic(&dir.path().join("credentials.json"), "{}").unwrap();
        for name in keep {
            assert!(dir.path().join(name).exists(), "{name} must survive");
        }
        assert!(
            fresh.exists(),
            "a fresh temp (live writer) must survive the age gate"
        );
    }

    #[test]
    #[cfg(unix)]
    fn write_private_atomic_sets_owner_only_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("secret");
        write_private_atomic(&path, "s").unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }
}
