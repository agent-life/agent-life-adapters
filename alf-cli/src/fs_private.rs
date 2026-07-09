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
        f.write_all(content.as_bytes())?;
        f.sync_all()?;
    }

    match std::fs::rename(&tmp, path) {
        Ok(()) => Ok(()),
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            Err(e)
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
