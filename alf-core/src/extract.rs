//! Symlink-safe writes for archive-controlled restore output.
//!
//! [`crate::safe_extract_path`] validates archive path spelling. This module
//! adds the filesystem half: beneath a trusted base, an archive path may not
//! traverse existing symlinks/reparse points or non-directory parents.

use std::fs;
use std::io::Write;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::archive::{safe_extract_path, ArchiveError};

/// Output policy for [`write_extracted_file`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ExtractWriteMode {
    /// Restore ordinary archive content with normal file permissions.
    #[default]
    Normal,
    /// Restore credential or vault material privately and atomically.
    PrivateAtomic,
}

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Write `bytes` below `base` without following archive-controlled
/// symlink/reparse-point path components.
///
/// `base` may itself be a symlink; its canonical target is the trusted
/// boundary. The pre-existing-symlink attack is rejected by walking every
/// component with `symlink_metadata` before opening a file. The final write is
/// made through a freshly-created sibling temporary file and renamed into the
/// validated parent. Descriptor-relative APIs are not portable in `std`, so a
/// hostile same-UID process can still race directory replacement between the
/// final check and rename on some platforms; callers must not treat this as a
/// complete concurrent-attacker sandbox.
pub fn write_extracted_file(
    base: &Path,
    relative: &str,
    bytes: &[u8],
    mode: ExtractWriteMode,
) -> Result<(), ArchiveError> {
    // Validate before any filesystem mutation. This also makes the accepted
    // component grammar exactly match all existing raw-entry validation.
    safe_extract_path(base, relative)?;

    fs::create_dir_all(base)?;
    let canonical_base = fs::canonicalize(base)?;
    if !is_ordinary_directory(&fs::symlink_metadata(&canonical_base)?) {
        return Err(ArchiveError::Invalid(format!(
            "unsafe extraction base: {} is not an ordinary directory",
            base.display()
        )));
    }

    let mut components: Vec<&str> = relative.split('/').filter(|part| *part != ".").collect();
    let file_name = components
        .pop()
        .ok_or_else(|| ArchiveError::Invalid(format!("unsafe archive entry path: {relative:?}")))?;

    let mut parent = canonical_base.clone();
    for component in components {
        let next = parent.join(component);
        match fs::symlink_metadata(&next) {
            Ok(metadata) => {
                if !is_ordinary_directory(&metadata) {
                    return Err(unsafe_node(relative, &next, "parent"));
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                match fs::create_dir(&next) {
                    Ok(()) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                    Err(error) => return Err(error.into()),
                }
                if !is_ordinary_directory(&fs::symlink_metadata(&next)?) {
                    return Err(unsafe_node(relative, &next, "parent"));
                }
            }
            Err(error) => return Err(error.into()),
        }
        parent = next;
    }

    // Resolve once more immediately before opening. This detects a parent that
    // was changed after the component walk and lets writes use the resolved
    // spelling rather than an attacker-controlled symlink spelling.
    let canonical_parent = fs::canonicalize(&parent)?;
    if !canonical_parent.starts_with(&canonical_base) {
        return Err(ArchiveError::Invalid(format!(
            "unsafe extraction parent for {relative:?}: {} escapes {}",
            canonical_parent.display(),
            canonical_base.display()
        )));
    }

    let target = canonical_parent.join(file_name);
    reject_unsafe_final(relative, &target)?;
    write_atomically(&target, bytes, mode)?;
    Ok(())
}

fn is_ordinary_directory(metadata: &fs::Metadata) -> bool {
    metadata.is_dir() && !is_link_or_reparse(metadata)
}

fn is_link_or_reparse(metadata: &fs::Metadata) -> bool {
    if metadata.file_type().is_symlink() {
        return true;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
        metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
    }
    #[cfg(not(windows))]
    {
        false
    }
}

fn unsafe_node(relative: &str, path: &Path, kind: &str) -> ArchiveError {
    ArchiveError::Invalid(format!(
        "unsafe extraction {kind} for {relative:?}: {}",
        path.display()
    ))
}

fn reject_unsafe_final(relative: &str, target: &Path) -> Result<(), ArchiveError> {
    match fs::symlink_metadata(target) {
        Ok(metadata) if is_link_or_reparse(&metadata) || !metadata.is_file() => {
            Err(unsafe_node(relative, target, "target"))
        }
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

/// Write sensitive archive-derived bytes to an explicitly selected target.
///
/// The target's parent is the trusted base and its final component is still
/// checked by [`write_extracted_file`]. This is for fixed runtime/vault targets
/// such as `auth_profiles.json`, not archive-controlled full paths.
pub fn write_private_extracted_target(target: &Path, bytes: &[u8]) -> Result<(), ArchiveError> {
    let parent = target.parent().ok_or_else(|| {
        ArchiveError::Invalid(format!(
            "unsafe extraction target: {} has no parent",
            target.display()
        ))
    })?;
    let name = target
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| {
            ArchiveError::Invalid(format!(
                "unsafe extraction target: {} has no file name",
                target.display()
            ))
        })?;
    write_extracted_file(parent, name, bytes, ExtractWriteMode::PrivateAtomic)
}
fn write_atomically(
    target: &Path,
    bytes: &[u8],
    mode: ExtractWriteMode,
) -> Result<(), ArchiveError> {
    let parent = target.parent().ok_or_else(|| {
        ArchiveError::Invalid(format!(
            "unsafe extraction target: {} has no parent",
            target.display()
        ))
    })?;
    let name = target
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| {
            ArchiveError::Invalid(format!(
                "unsafe extraction target: {} has no file name",
                target.display()
            ))
        })?;

    let temporary = loop {
        let candidate = parent.join(format!(
            ".{name}.alf-extract-tmp.{}.{}",
            std::process::id(),
            TEMP_COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        add_private_mode(&mut options, mode);
        match options.open(&candidate) {
            Ok(mut file) => {
                file.write_all(bytes)?;
                file.sync_all()?;
                break candidate;
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error.into()),
        }
    };

    // Do not silently replace a pre-existing final symlink. On POSIX rename
    // would replace the link itself rather than following it, but surfacing an
    // unsafe-extraction error is the contract callers rely on.
    let result = (|| -> Result<(), ArchiveError> {
        reject_unsafe_final("private archive output", target)?;
        replace_file(&temporary, target)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

#[cfg(not(windows))]
fn replace_file(temporary: &Path, target: &Path) -> std::io::Result<()> {
    fs::rename(temporary, target)
}

#[cfg(windows)]
fn replace_file(temporary: &Path, target: &Path) -> std::io::Result<()> {
    if target.exists() {
        // `rename` does not replace an existing file on Windows. The caller
        // has just rejected a final symlink/reparse point, so removing it here
        // cannot follow the archive target outside the validated parent.
        fs::remove_file(target)?;
    }
    fs::rename(temporary, target)
}
#[cfg(unix)]
fn add_private_mode(options: &mut fs::OpenOptions, mode: ExtractWriteMode) {
    if mode == ExtractWriteMode::PrivateAtomic {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
}

#[cfg(not(unix))]
fn add_private_mode(_: &mut fs::OpenOptions, _: ExtractWriteMode) {}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn writes_safe_nested_file() {
        let dir = TempDir::new().unwrap();
        write_extracted_file(
            dir.path(),
            "memory/2026-07-30.md",
            b"safe",
            ExtractWriteMode::Normal,
        )
        .unwrap();
        assert_eq!(
            fs::read(dir.path().join("memory/2026-07-30.md")).unwrap(),
            b"safe"
        );
    }

    #[test]
    fn rejects_lexical_traversal_without_creating_base() {
        let dir = TempDir::new().unwrap();
        let base = dir.path().join("workspace");
        assert!(
            write_extracted_file(&base, "../outside", b"no", ExtractWriteMode::Normal).is_err()
        );
        assert!(!base.exists());
    }

    #[test]
    fn rejects_intermediate_file() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("memory"), b"not a directory").unwrap();
        assert!(write_extracted_file(
            dir.path(),
            "memory/today.md",
            b"no",
            ExtractWriteMode::Normal
        )
        .is_err());
    }

    #[test]
    #[cfg(unix)]
    fn rejects_symlinked_parent_and_final_target() {
        use std::os::unix::fs::symlink;

        let dir = TempDir::new().unwrap();
        let outside = TempDir::new().unwrap();
        symlink(outside.path(), dir.path().join("link")).unwrap();
        assert!(
            write_extracted_file(dir.path(), "link/pwn.txt", b"no", ExtractWriteMode::Normal)
                .is_err()
        );
        assert!(!outside.path().join("pwn.txt").exists());

        let sentinel = outside.path().join("sentinel.txt");
        fs::write(&sentinel, b"keep").unwrap();
        symlink(&sentinel, dir.path().join("target.txt")).unwrap();
        assert!(
            write_extracted_file(dir.path(), "target.txt", b"no", ExtractWriteMode::Normal)
                .is_err()
        );
        assert_eq!(fs::read(&sentinel).unwrap(), b"keep");
    }

    #[test]
    fn overwrites_an_ordinary_file() {
        let dir = TempDir::new().unwrap();
        let target = dir.path().join("target.txt");
        fs::write(&target, b"old").unwrap();
        write_extracted_file(dir.path(), "target.txt", b"new", ExtractWriteMode::Normal).unwrap();
        assert_eq!(fs::read(target).unwrap(), b"new");
    }

    #[test]
    #[cfg(unix)]
    fn private_output_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;

        let dir = TempDir::new().unwrap();
        write_extracted_file(
            dir.path(),
            "secret.json",
            b"secret",
            ExtractWriteMode::PrivateAtomic,
        )
        .unwrap();
        let mode = fs::metadata(dir.path().join("secret.json"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600);
    }
}
