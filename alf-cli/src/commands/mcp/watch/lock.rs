//! Per-agent advisory lock (design §11.4, brief task 6; manual §6).
//!
//! ## Lock hierarchy (v1.1, manual §6)
//! Level 1: the in-process `write_lock` (mcp/mod.rs) — config/map/include/vault
//! RMW tools. Level 2: the in-process `sync_lock` (tokio) — whole-workspace ops
//! (`alf_sync`, head `alf_restore`, the watch loop's sync). Level 3: THIS
//! cross-process flock — watch syncs, manual syncs, head restores, and vault
//! mutations. Acquisition order is always L1/L2 → L3; the include-list flock
//! (`.alf-include.lock`, [`acquire_blocking`]) is INNERMOST — never acquire
//! this per-agent lock while holding it.
//!
//! Since the MAJ-6 fix, plain-CLI `alf sync` and head `alf restore` take this
//! lock too (a deliberate goal-c deviation): a CLI mutation must not
//! interleave with a watch-loop export on the same agent — the sequence CAS
//! arbitrates ordering across machines (case E7), but cannot stop a torn
//! same-host read. Uncontended (no MCP server running) the acquisition is one
//! open+flock; contended callers get `agent_busy` after a bounded wait.
//!
//! The lock is an exclusive `flock` on `~/.alf/state/{agent_id}.lock`, released
//! when the guard drops (or the process dies — the kernel drops flocks on close,
//! so a SIGKILL cannot strand the lock; design §5.3 crash-safety).

use std::fs::{File, OpenOptions};
use std::io;
use std::path::Path;
use std::time::{Duration, Instant};

use fs2::FileExt;

/// An held advisory lock. Releasing on drop is the whole point — do not
/// `mem::forget` it.
#[derive(Debug)]
pub struct AgentLock {
    _file: File,
}

/// True when `e` is the platform's "lock held by someone else" error — fs2 maps
/// contention to `WouldBlock` on most platforms, but some surface the raw errno,
/// so compare against [`fs2::lock_contended_error`] too. Anything else (ENOLCK /
/// EOPNOTSUPP on NFS/FUSE/SMB homes, EIO, …) means the lock is UNUSABLE, not
/// busy — mapping those to "contended" made the watch loop silently never sync
/// and every tool report a phantom `agent_busy` on such filesystems.
fn contended(e: &io::Error) -> bool {
    e.kind() == io::ErrorKind::WouldBlock
        || (e.raw_os_error().is_some()
            && e.raw_os_error() == fs2::lock_contended_error().raw_os_error())
}

/// Try to acquire the lock at `lock_path` without blocking.
///
/// - `Ok(Some(guard))` — acquired; hold it for the sync/restore.
/// - `Ok(None)` — another process holds it; skip this tick.
/// - `Err(_)` — the lock file could not be created (permissions, missing dir) or
///   the filesystem cannot take the lock (no flock support). Callers treat this
///   as a real failure: the watch loop's strike counter parks on it, tools fail
///   fast instead of waiting out their bounded poll.
pub fn try_acquire(lock_path: &Path) -> io::Result<Option<AgentLock>> {
    let file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(lock_path)?;
    match file.try_lock_exclusive() {
        Ok(()) => Ok(Some(AgentLock { _file: file })),
        Err(e) if contended(&e) => Ok(None),
        Err(e) => Err(e),
    }
}

/// Acquire with a bounded wait: poll [`try_acquire`] every `poll` until
/// `timeout`. `Ok(None)` means the holder outlasted the wait — the caller
/// surfaces `agent_busy` (manual §6) rather than blocking forever.
pub fn acquire_timeout(
    lock_path: &Path,
    timeout: Duration,
    poll: Duration,
) -> io::Result<Option<AgentLock>> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(guard) = try_acquire(lock_path)? {
            return Ok(Some(guard));
        }
        if Instant::now() >= deadline {
            return Ok(None);
        }
        std::thread::sleep(poll.min(deadline.saturating_duration_since(Instant::now())));
    }
}

/// Blocking exclusive acquire — for short critical sections (the include-list
/// RMW). LOCK ORDER: this is the INNERMOST lock; never acquire the per-agent
/// sync lock while holding it.
pub fn acquire_blocking(lock_path: &Path) -> io::Result<AgentLock> {
    let file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(lock_path)?;
    file.lock_exclusive()?;
    Ok(AgentLock { _file: file })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn second_acquire_is_contended_until_first_drops() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("agent.lock");

        let first = try_acquire(&path).unwrap();
        assert!(first.is_some(), "first acquire succeeds");

        // A second exclusive lock on the same file (separate fd) is contended.
        let second = try_acquire(&path).unwrap();
        assert!(
            second.is_none(),
            "second acquire is blocked while first held"
        );

        drop(first);
        // Once released, it can be taken again.
        let third = try_acquire(&path).unwrap();
        assert!(third.is_some(), "lock is reusable after release");
    }

    #[test]
    fn missing_dir_errors() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("no-such-dir").join("agent.lock");
        assert!(try_acquire(&path).is_err());
    }

    #[test]
    fn contended_matches_only_the_platform_contention_error() {
        // The two shapes real contention arrives in.
        assert!(contended(&io::Error::from(io::ErrorKind::WouldBlock)));
        assert!(contended(&fs2::lock_contended_error()));
        // Unusable-lock errors must NOT read as contention: a filesystem
        // without flock support (ENOLCK/EOPNOTSUPP — NFS, some FUSE/SMB homes)
        // previously made the loop skip every tick forever and the tools
        // report a phantom agent_busy.
        assert!(!contended(&io::Error::new(
            io::ErrorKind::Unsupported,
            "flock unsupported"
        )));
        assert!(!contended(&io::Error::from(
            io::ErrorKind::PermissionDenied
        )));
        assert!(!contended(&io::Error::other("I/O error")));
    }

    #[test]
    fn acquire_timeout_waits_for_release() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("agent.lock");
        let held = try_acquire(&path).unwrap().unwrap();
        let path2 = path.clone();
        let holder = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(300));
            drop(held);
        });
        let got =
            acquire_timeout(&path2, Duration::from_secs(2), Duration::from_millis(25)).unwrap();
        assert!(
            got.is_some(),
            "acquire_timeout must win once the holder drops"
        );
        holder.join().unwrap();
    }

    #[test]
    fn acquire_timeout_times_out_while_held() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("agent.lock");
        let _held = try_acquire(&path).unwrap().unwrap();
        let got =
            acquire_timeout(&path, Duration::from_millis(150), Duration::from_millis(25)).unwrap();
        assert!(got.is_none(), "a persistent holder times the waiter out");
    }

    #[test]
    fn acquire_blocking_waits_for_release() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("include.lock");
        let held = acquire_blocking(&path).unwrap();
        let path2 = path.clone();
        let holder = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(100));
            drop(held);
        });
        let _got = acquire_blocking(&path2).unwrap(); // blocks until the drop
        holder.join().unwrap();
    }
}
