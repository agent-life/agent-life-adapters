//! Per-agent advisory lock (design §11.4, brief task 6).
//!
//! There is no local lock anywhere else in `alf-cli` — same-agent sync races are
//! arbitrated only by the service's atomic sequence CAS (case E7). This lock does
//! **not** change that contract for the CLI (goal c): it protects
//! **MCP-server-vs-MCP-server** on one machine so two ALF-aware processes pinned
//! to the same agent coordinate voluntarily around a sync/restore, rather than
//! both racing to the 409. Held only for the duration of a sync/restore; a lock
//! held by another process means "someone else is syncing this agent" → skip the
//! tick.
//!
//! The lock is an exclusive `flock` on `~/.alf/state/{agent_id}.lock`, released
//! when the guard drops (or the process dies — the kernel drops flocks on close,
//! so a SIGKILL cannot strand the lock; design §5.3 crash-safety).

use std::fs::{File, OpenOptions};
use std::io;
use std::path::Path;

use fs2::FileExt;

/// An held advisory lock. Releasing on drop is the whole point — do not
/// `mem::forget` it.
#[derive(Debug)]
pub struct AgentLock {
    _file: File,
}

/// Try to acquire the lock at `lock_path` without blocking.
///
/// - `Ok(Some(guard))` — acquired; hold it for the sync/restore.
/// - `Ok(None)` — another process holds it; skip this tick.
/// - `Err(_)` — the lock file could not be created (permissions, missing dir).
pub fn try_acquire(lock_path: &Path) -> io::Result<Option<AgentLock>> {
    let file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(lock_path)?;
    match file.try_lock_exclusive() {
        Ok(()) => Ok(Some(AgentLock { _file: file })),
        Err(e) if e.kind() == io::ErrorKind::WouldBlock => Ok(None),
        // fs2 maps a contended lock to WouldBlock; some platforms surface it as a
        // raw errno. Treat any lock error as "contended, skip" rather than fatal.
        Err(_) => Ok(None),
    }
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
}
