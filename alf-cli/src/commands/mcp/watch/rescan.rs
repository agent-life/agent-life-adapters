//! Bounded, deterministic polling fingerprints for the MCP watch loop.
//!
//! This is the correctness backstop for missed or unavailable OS notifications.
//! Incomplete scans are deliberately non-authoritative: they never replace a
//! previous complete fingerprint.

use std::collections::BTreeMap;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use alf_core::WatchSpec;
use sha2::{Digest, Sha256};

const DIGEST_BUFFER: usize = 64 * 1024;

/// How many consecutive degraded scans pass before a still-degraded source is
/// dirtied again. A source whose tree cannot complete inside one tick fails on
/// every tick, so dirtying only on the transition into degraded would leave it
/// silently unpolled forever after a single sync. Re-dirtying on a bounded
/// cadence keeps the backstop honest without marking it dirty every tick.
const DEGRADED_REDIRTY_SCANS: u32 = 60;

#[derive(Clone, PartialEq, Eq)]
pub(super) struct PollingIssue {
    pub(super) source: String,
    pub(super) code: String,
    pub(super) message: String,
}

#[derive(Clone, Copy)]
pub(super) struct RescanBudget {
    pub(super) max_entries: usize,
    pub(super) max_bytes: u64,
    pub(super) max_tick: Duration,
}

#[derive(Clone, PartialEq, Eq)]
struct SourceFingerprint {
    entries: Vec<FingerprintEntry>,
}

#[derive(Clone, PartialEq, Eq)]
struct FingerprintEntry {
    root: PathBuf,
    relative: PathBuf,
    kind: EntryKind,
    size: u64,
    modified: Option<SystemTime>,
    digest: Option<[u8; 32]>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum EntryKind {
    Missing,
    File,
    Dir,
    Symlink,
    Other,
}

struct ScanUse {
    entries: usize,
    bytes: u64,
    max_entries: usize,
    max_bytes: u64,
    deadline: Instant,
}

impl ScanUse {
    fn check_time(&self) -> Result<(), ScanFailure> {
        if Instant::now() >= self.deadline {
            Err(ScanFailure::time_limit())
        } else {
            Ok(())
        }
    }

    fn entry(&mut self) -> Result<(), ScanFailure> {
        self.check_time()?;
        if self.entries >= self.max_entries {
            return Err(ScanFailure::entry_limit());
        }
        self.entries += 1;
        Ok(())
    }

    /// `Ok(None)` means the file vanished between `read_dir` and `open`. Editor
    /// temp files and SQLite sidecars churn constantly, so that is an ordinary
    /// missing entry — not a failure that should degrade the whole source.
    /// Mirrors the `NotFound` arm of the stat path.
    fn hash_file(&mut self, path: &Path, size: u64) -> Result<Option<[u8; 32]>, ScanFailure> {
        self.check_time()?;
        if self.bytes.saturating_add(size) > self.max_bytes {
            return Err(ScanFailure::byte_limit());
        }

        let mut file = match fs::File::open(path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(ScanFailure::io(path, error)),
        };
        let mut hasher = Sha256::new();
        let mut buffer = [0_u8; DIGEST_BUFFER];
        loop {
            self.check_time()?;
            let read = file
                .read(&mut buffer)
                .map_err(|e| ScanFailure::io(path, e))?;
            if read == 0 {
                break;
            }
            self.bytes = self.bytes.saturating_add(read as u64);
            if self.bytes > self.max_bytes {
                return Err(ScanFailure::byte_limit());
            }
            hasher.update(&buffer[..read]);
        }
        Ok(Some(hasher.finalize().into()))
    }
}

struct ScanFailure {
    code: &'static str,
    message: String,
}

impl ScanFailure {
    fn entry_limit() -> Self {
        Self {
            code: "scan_entry_limit",
            message: "recursive polling reached its entry limit".into(),
        }
    }

    fn byte_limit() -> Self {
        Self {
            code: "scan_byte_limit",
            message: "recursive polling reached its content-hash byte limit".into(),
        }
    }

    fn time_limit() -> Self {
        Self {
            code: "scan_time_limit",
            message: "recursive polling reached its wall-time limit".into(),
        }
    }

    fn io(path: &Path, error: std::io::Error) -> Self {
        Self {
            code: "scan_io_error",
            message: format!("cannot scan {}: {}", path.display(), sanitize_error(error)),
        }
    }
}

fn sanitize_error(error: impl std::fmt::Display) -> String {
    let mut message = error.to_string().replace(['\n', '\r'], " ");
    const MAX_ERROR_CHARS: usize = 320;
    if message.chars().count() > MAX_ERROR_CHARS {
        message = format!(
            "{}…",
            message.chars().take(MAX_ERROR_CHARS).collect::<String>()
        );
    }
    message
}

/// Cached full-source fingerprints. A source can have several roots, so all
/// specs with the same source ID are fingerprinted and compared together.
pub(super) struct FingerprintCache {
    definitions: BTreeMap<String, Vec<WatchSpec>>,
    complete: BTreeMap<String, SourceFingerprint>,
    degraded: BTreeMap<String, DegradedSource>,
    next_source: usize,
}

/// A source whose latest scan did not complete, plus how many scans have passed
/// since it was last dirtied (see [`DEGRADED_REDIRTY_SCANS`]).
struct DegradedSource {
    issue: PollingIssue,
    scans_since_dirty: u32,
}

impl FingerprintCache {
    pub(super) fn new(specs: &[WatchSpec]) -> Self {
        Self {
            definitions: group_specs(specs),
            complete: BTreeMap::new(),
            degraded: BTreeMap::new(),
            next_source: 0,
        }
    }

    /// Update cached source definitions after a surface refresh. A changed
    /// definition invalidates its baseline and dirties the source once.
    pub(super) fn reconcile(&mut self, specs: &[WatchSpec]) -> Vec<String> {
        let next = group_specs(specs);
        let mut changed = Vec::new();
        for (source, definition) in &next {
            if self.definitions.get(source) != Some(definition) {
                self.complete.remove(source);
                self.degraded.remove(source);
                changed.push(source.clone());
            }
        }
        self.complete.retain(|source, _| next.contains_key(source));
        self.degraded.retain(|source, _| next.contains_key(source));
        self.definitions = next;
        if self.definitions.is_empty() {
            self.next_source = 0;
        } else {
            self.next_source %= self.definitions.len();
        }
        changed
    }

    /// Scan sources in round-robin order until the tick deadline. Only complete
    /// scans update a baseline; incomplete scans remain visible and dirty the
    /// source on their first transition to degraded.
    pub(super) fn rescan(&mut self, budget: RescanBudget) -> Vec<String> {
        let source_ids: Vec<String> = self.definitions.keys().cloned().collect();
        if source_ids.is_empty() {
            return Vec::new();
        }

        let deadline = Instant::now()
            .checked_add(budget.max_tick)
            .unwrap_or_else(Instant::now);
        let start = self.next_source % source_ids.len();
        let mut changed = Vec::new();

        for offset in 0..source_ids.len() {
            if Instant::now() >= deadline {
                break;
            }
            let index = (start + offset) % source_ids.len();
            self.next_source = (index + 1) % source_ids.len();
            let source = &source_ids[index];
            let specs = self
                .definitions
                .get(source)
                .expect("source id came from definitions");

            match fingerprint_source(specs, budget, deadline) {
                Ok(fingerprint) => {
                    let recovered = self.degraded.remove(source).is_some();
                    match self.complete.get(source) {
                        Some(previous) if previous != &fingerprint => {
                            self.complete.insert(source.clone(), fingerprint);
                            changed.push(source.clone());
                        }
                        Some(_) => {}
                        None => {
                            self.complete.insert(source.clone(), fingerprint);
                            if recovered {
                                changed.push(source.clone());
                            }
                        }
                    }
                }
                Err(failure) => {
                    let issue = PollingIssue {
                        source: source.clone(),
                        code: failure.code.into(),
                        message: failure.message,
                    };
                    match self.degraded.get_mut(source) {
                        // Still failing the same way: stay quiet, but do not go
                        // silent forever — a tree that can never complete would
                        // otherwise drop out of the backstop after one sync.
                        Some(existing) if existing.issue == issue => {
                            existing.scans_since_dirty += 1;
                            if existing.scans_since_dirty >= DEGRADED_REDIRTY_SCANS {
                                existing.scans_since_dirty = 0;
                                changed.push(source.clone());
                            }
                        }
                        _ => {
                            self.degraded.insert(
                                source.clone(),
                                DegradedSource {
                                    issue,
                                    scans_since_dirty: 0,
                                },
                            );
                            changed.push(source.clone());
                        }
                    }
                }
            }
        }
        changed.sort();
        changed.dedup();
        changed
    }

    pub(super) fn degraded_sources(&self) -> Vec<PollingIssue> {
        self.degraded
            .values()
            .map(|state| state.issue.clone())
            .collect()
    }
}

fn group_specs(specs: &[WatchSpec]) -> BTreeMap<String, Vec<WatchSpec>> {
    let mut grouped = BTreeMap::<String, Vec<WatchSpec>>::new();
    for spec in specs {
        grouped
            .entry(spec.id.clone())
            .or_default()
            .push(spec.clone());
    }
    grouped
}

fn fingerprint_source(
    specs: &[WatchSpec],
    budget: RescanBudget,
    deadline: Instant,
) -> Result<SourceFingerprint, ScanFailure> {
    let mut use_budget = ScanUse {
        entries: 0,
        bytes: 0,
        max_entries: budget.max_entries,
        max_bytes: budget.max_bytes,
        deadline,
    };
    let mut entries = Vec::new();

    for spec in specs {
        for root in &spec.roots {
            scan_path(
                root,
                root,
                Path::new(""),
                spec.recursive,
                /* is_root */ true,
                &spec.exclude,
                &mut use_budget,
                &mut entries,
            )?;
        }
    }

    entries.sort_by(|left, right| {
        left.root
            .cmp(&right.root)
            .then_with(|| left.relative.cmp(&right.relative))
            .then_with(|| entry_kind_rank(left.kind).cmp(&entry_kind_rank(right.kind)))
    });
    Ok(SourceFingerprint { entries })
}

fn entry_kind_rank(kind: EntryKind) -> u8 {
    match kind {
        EntryKind::Missing => 0,
        EntryKind::File => 1,
        EntryKind::Dir => 2,
        EntryKind::Symlink => 3,
        EntryKind::Other => 4,
    }
}

fn missing_entry(root: &Path, relative: &Path) -> FingerprintEntry {
    FingerprintEntry {
        root: root.to_path_buf(),
        relative: relative.to_path_buf(),
        kind: EntryKind::Missing,
        size: 0,
        modified: None,
        digest: None,
    }
}

#[allow(clippy::too_many_arguments)]
fn scan_path(
    root: &Path,
    path: &Path,
    relative: &Path,
    recursive: bool,
    is_root: bool,
    excludes: &[PathBuf],
    use_budget: &mut ScanUse,
    entries: &mut Vec<FingerprintEntry>,
) -> Result<(), ScanFailure> {
    if excludes.iter().any(|exclude| path.starts_with(exclude)) {
        return Ok(());
    }

    use_budget.entry()?;
    let mut metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            entries.push(missing_entry(root, relative));
            return Ok(());
        }
        Err(error) => return Err(ScanFailure::io(path, error)),
    };

    // A spec root may legitimately BE a symlink to the real directory — a
    // dotfile-managed install points `~/.openclaw` elsewhere, and notify
    // resolves it the same way. Resolve it once, at the root only: otherwise
    // the whole subtree fingerprints as a single link entry, the scan
    // *succeeds*, and every change inside it reads as clean forever.
    // Descendants are never followed, which keeps traversal cycle-free.
    if is_root && metadata.file_type().is_symlink() {
        metadata = match fs::metadata(path) {
            Ok(resolved) => resolved,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                entries.push(missing_entry(root, relative));
                return Ok(());
            }
            Err(error) => return Err(ScanFailure::io(path, error)),
        };
    }

    let file_type = metadata.file_type();
    let kind = if file_type.is_file() {
        EntryKind::File
    } else if file_type.is_dir() {
        EntryKind::Dir
    } else if file_type.is_symlink() {
        EntryKind::Symlink
    } else {
        EntryKind::Other
    };
    let digest = if kind == EntryKind::File {
        match use_budget.hash_file(path, metadata.len())? {
            Some(digest) => Some(digest),
            // Vanished between `read_dir` and `open`: record the absence rather
            // than degrading every other entry in this source.
            None => {
                entries.push(missing_entry(root, relative));
                return Ok(());
            }
        }
    } else {
        None
    };
    entries.push(FingerprintEntry {
        root: root.to_path_buf(),
        relative: relative.to_path_buf(),
        kind,
        size: metadata.len(),
        modified: metadata.modified().ok(),
        digest,
    });

    if !recursive || kind != EntryKind::Dir {
        return Ok(());
    }

    let mut children = Vec::new();
    for child in fs::read_dir(path).map_err(|error| ScanFailure::io(path, error))? {
        use_budget.check_time()?;
        let child = child.map_err(|error| ScanFailure::io(path, error))?;
        let child_path = child.path();
        if excludes
            .iter()
            .any(|exclude| child_path.starts_with(exclude))
        {
            continue;
        }
        if children.len() >= use_budget.max_entries.saturating_sub(use_budget.entries) {
            return Err(ScanFailure::entry_limit());
        }
        children.push((child.file_name(), child_path));
    }
    children.sort_by(|left, right| left.1.cmp(&right.1));

    for (name, child) in children {
        scan_path(
            root,
            &child,
            &relative.join(name),
            true,
            /* is_root */ false,
            excludes,
            use_budget,
            entries,
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn budget(entries: usize) -> RescanBudget {
        RescanBudget {
            max_entries: entries,
            max_bytes: 64 * 1024 * 1024,
            max_tick: Duration::from_secs(1),
        }
    }

    #[test]
    fn nested_edit_changes_recursive_source_even_when_root_mtime_is_unchanged() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("memory");
        let nested = root.join("a/b/note.md");
        fs::create_dir_all(nested.parent().unwrap()).unwrap();
        fs::write(&nested, "first").unwrap();
        let root_time = fs::metadata(&root).unwrap().modified().unwrap();

        let spec = WatchSpec::dir("memory", root.clone());
        let mut cache = FingerprintCache::new(&[spec]);
        assert!(cache.rescan(budget(100)).is_empty());

        fs::write(&nested, "second").unwrap();
        fs::File::open(&root)
            .unwrap()
            .set_times(fs::FileTimes::new().set_modified(root_time))
            .unwrap();

        assert_eq!(cache.rescan(budget(100)), vec!["memory"]);
    }

    #[test]
    fn same_length_rewrite_with_restored_mtime_changes_digest() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("note.md");
        fs::write(&file, "first").unwrap();
        let file_time = fs::metadata(&file).unwrap().modified().unwrap();

        let spec = WatchSpec::file("note", file.clone());
        let mut cache = FingerprintCache::new(&[spec]);
        assert!(cache.rescan(budget(10)).is_empty());

        fs::write(&file, "other").unwrap();
        fs::File::open(&file)
            .unwrap()
            .set_times(fs::FileTimes::new().set_modified(file_time))
            .unwrap();

        assert_eq!(cache.rescan(budget(10)), vec!["note"]);
    }

    #[test]
    fn excluded_paths_are_not_fingerprinted() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("memory");
        let excluded = root.join(".git/ignored");
        fs::create_dir_all(excluded.parent().unwrap()).unwrap();
        fs::write(&excluded, "first").unwrap();

        let spec = WatchSpec::dir("memory", root.clone()).excluding([root.join(".git")]);
        let mut cache = FingerprintCache::new(&[spec]);
        assert!(cache.rescan(budget(100)).is_empty());

        fs::write(&excluded, "second").unwrap();
        assert!(cache.rescan(budget(100)).is_empty());
    }

    #[test]
    fn over_budget_scan_is_degraded_and_never_becomes_a_clean_baseline() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("memory");
        fs::create_dir_all(&root).unwrap();
        for n in 0..3 {
            fs::write(root.join(format!("{n}.md")), "x").unwrap();
        }

        let spec = WatchSpec::dir("memory", root);
        let mut cache = FingerprintCache::new(&[spec]);
        assert_eq!(cache.rescan(budget(2)), vec!["memory"]);
        assert_eq!(cache.degraded_sources().len(), 1);

        assert_eq!(cache.rescan(budget(100)), vec!["memory"]);
        assert!(cache.degraded_sources().is_empty());
    }

    #[test]
    fn nested_create_rename_and_delete_each_change_the_fingerprint() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("memory");
        let nested = root.join("a");
        fs::create_dir_all(&nested).unwrap();
        fs::write(nested.join("one.md"), "one").unwrap();

        let spec = WatchSpec::dir("memory", root);
        let mut cache = FingerprintCache::new(&[spec]);
        assert!(cache.rescan(budget(100)).is_empty());

        fs::write(nested.join("two.md"), "two").unwrap();
        assert_eq!(cache.rescan(budget(100)), vec!["memory"], "nested create");

        fs::rename(nested.join("two.md"), nested.join("three.md")).unwrap();
        assert_eq!(cache.rescan(budget(100)), vec!["memory"], "nested rename");

        fs::remove_file(nested.join("three.md")).unwrap();
        assert_eq!(cache.rescan(budget(100)), vec!["memory"], "nested delete");

        assert!(
            cache.rescan(budget(100)).is_empty(),
            "a settled tree stays clean"
        );
    }

    #[test]
    #[cfg(unix)]
    fn symlinked_root_is_fingerprinted_through_to_its_target() {
        use std::os::unix::fs::symlink;

        let tmp = tempfile::tempdir().unwrap();
        let real = tmp.path().join("real-memory");
        let nested = real.join("a/note.md");
        fs::create_dir_all(nested.parent().unwrap()).unwrap();
        fs::write(&nested, "first").unwrap();
        let link = tmp.path().join("memory");
        symlink(&real, &link).unwrap();

        let spec = WatchSpec::dir("memory", link);
        let mut cache = FingerprintCache::new(&[spec]);
        assert!(cache.rescan(budget(100)).is_empty());
        assert!(
            cache.degraded_sources().is_empty(),
            "a symlinked root must be scanned, not degraded"
        );

        fs::write(&nested, "second").unwrap();
        assert_eq!(
            cache.rescan(budget(100)),
            vec!["memory"],
            "an edit behind a symlinked root must not read as clean"
        );
    }

    #[test]
    #[cfg(unix)]
    fn dangling_root_symlink_is_recorded_missing_without_degrading() {
        use std::os::unix::fs::symlink;

        let tmp = tempfile::tempdir().unwrap();
        let link = tmp.path().join("memory");
        symlink(tmp.path().join("nowhere"), &link).unwrap();

        let spec = WatchSpec::dir("memory", link);
        let mut cache = FingerprintCache::new(&[spec]);
        assert!(cache.rescan(budget(10)).is_empty());
        assert!(cache.degraded_sources().is_empty());
    }

    #[test]
    fn vanished_file_yields_no_digest_instead_of_degrading_the_source() {
        let tmp = tempfile::tempdir().unwrap();
        let mut use_budget = ScanUse {
            entries: 0,
            bytes: 0,
            max_entries: 10,
            max_bytes: 1024,
            deadline: Instant::now() + Duration::from_secs(1),
        };

        let vanished = use_budget.hash_file(&tmp.path().join("gone.md"), 5);
        assert!(
            matches!(vanished, Ok(None)),
            "a file that disappears mid-scan is a missing entry, not a scan failure"
        );
    }

    #[test]
    fn degraded_source_is_redirtied_on_a_bounded_cadence() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("memory");
        fs::create_dir_all(&root).unwrap();
        for n in 0..3 {
            fs::write(root.join(format!("{n}.md")), "x").unwrap();
        }

        let spec = WatchSpec::dir("memory", root);
        let mut cache = FingerprintCache::new(&[spec]);

        // The transition into degraded dirties once...
        assert_eq!(cache.rescan(budget(2)), vec!["memory"]);
        // ...then it stays quiet for a bounded run of scans...
        for scan in 1..DEGRADED_REDIRTY_SCANS {
            assert!(
                cache.rescan(budget(2)).is_empty(),
                "scan {scan} must not re-dirty before the cadence elapses"
            );
        }
        // ...and re-dirties, so a tree that can never complete is never
        // silently dropped from the backstop.
        assert_eq!(cache.rescan(budget(2)), vec!["memory"]);
        assert_eq!(cache.degraded_sources().len(), 1);
    }
}
