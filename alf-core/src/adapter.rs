//! Adapter trait and report types shared across all framework adapters.
//!
//! Each agent framework (OpenClaw, ZeroClaw, etc.) implements the [`Adapter`]
//! trait. The CLI dispatches to the correct adapter based on the `--runtime`
//! flag.

use anyhow::{bail, Result};
use serde::Serialize;
use std::path::Path;

use crate::crypto::VaultKey;

// ---------------------------------------------------------------------------
// Export / Import reports
// ---------------------------------------------------------------------------

/// Summary of an export operation.
#[derive(Debug)]
pub struct ExportReport {
    pub agent_name: String,
    pub alf_version: String,
    pub memory_records: u64,
    pub identity_version: Option<u32>,
    pub principals_count: u32,
    pub credentials_count: u32,
    pub attachments_count: u32,
    pub raw_sources: Vec<String>,
    pub output_path: String,
    pub output_size_bytes: u64,
    /// Number of workspace files dropped by a `.alfignore` filter (0 when no
    /// `.alfignore` is present).
    pub excluded_by_alfignore: u32,
    /// Paths in the agent's include list (`alf add`) that no longer exist on
    /// disk at export time. `alf sync` prunes these and logs the removal.
    pub missing_includes: Vec<String>,
    /// Non-fatal advisories surfaced to the user on export/sync — e.g. the
    /// Hermes adapter's "`~/.hermes/.env` has N keys not backed up; vault them
    /// with `alf vault add`" notice (D4). Empty for adapters that emit none.
    pub warnings: Vec<String>,
}

/// Summary of an import operation.
#[derive(Debug)]
pub struct ImportReport {
    pub agent_name: String,
    pub memory_records: u64,
    pub identity_imported: bool,
    pub principals_count: u32,
    pub credentials_count: u32,
    pub warnings: Vec<String>,
}

// ---------------------------------------------------------------------------
// Import options
// ---------------------------------------------------------------------------

/// Optional inputs for an import run.
///
/// `vault_key`, when supplied, tells the adapter to decrypt
/// `CredentialRecord.encrypted_payload` entries and inject the resulting
/// plaintext into the target runtime's native credential storage. When
/// absent, adapters preserve the legacy behavior of reporting credentials
/// without writing them.
#[derive(Default)]
pub struct ImportOptions<'a> {
    pub vault_key: Option<&'a VaultKey>,
}

// ---------------------------------------------------------------------------
// Dry-run enumeration
// ---------------------------------------------------------------------------

/// A single file in an export or restore preview.
///
/// `path` is the path as it appears in the archive's `raw/{runtime}/` tree —
/// workspace-relative for files that live under the workspace, or a synthesized
/// name (e.g. ZeroClaw's redacted `config.toml`) for entries that do not.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FileEntry {
    pub path: String,
    pub size: u64,
}

/// Result of enumerating a workspace for an `export --dry-run` preview.
#[derive(Debug)]
pub struct WorkspaceEnumeration {
    pub agent_name: String,
    pub memory_records: u64,
    pub files: Vec<FileEntry>,
    pub excluded_by_alfignore: u32,
    pub total_size: u64,
    pub warnings: Vec<String>,
}

/// Result of enumerating an archive for a `restore --dry-run` preview.
#[derive(Debug)]
pub struct ArchiveEnumeration {
    pub files: Vec<FileEntry>,
    pub warnings: Vec<String>,
}

// ---------------------------------------------------------------------------
// Adapter trait
// ---------------------------------------------------------------------------

/// Trait that each runtime adapter must implement.
///
/// An adapter knows how to read a framework's native workspace format and
/// translate it to/from an ALF archive.
pub trait Adapter {
    /// Runtime identifier (e.g., `"openclaw"`, `"zeroclaw"`).
    fn name(&self) -> &str;

    /// Human-readable description of the adapter.
    fn description(&self) -> &str;

    /// Export a workspace to an .alf file.
    ///
    /// `workspace` is the path to the framework's workspace directory;
    /// `output` is the path to write the .alf file. Layer 4 (credentials)
    /// comes from the agent's explicit ALF vault (`~/.alf/vault/`) — export
    /// never reads a vault key.
    fn export(&self, workspace: &Path, output: &Path) -> Result<ExportReport>;

    /// Import an .alf file into a workspace with no options.
    fn import(&self, alf_file: &Path, workspace: &Path) -> Result<ImportReport> {
        self.import_with_options(alf_file, workspace, ImportOptions::default())
    }

    /// Import an .alf file into a workspace with caller-supplied options.
    fn import_with_options(
        &self,
        alf_file: &Path,
        workspace: &Path,
        options: ImportOptions<'_>,
    ) -> Result<ImportReport>;

    /// Enumerate the files an `export` would archive, without writing anything.
    ///
    /// Backs `alf export --dry-run`. Adapters that support dry-run override
    /// this; the default rejects the call so the CLI surfaces a clear error.
    fn enumerate_workspace(&self, _workspace: &Path) -> Result<WorkspaceEnumeration> {
        bail!("dry-run not supported for this runtime")
    }

    /// Enumerate the files an `import` would write from an archive, without
    /// touching the filesystem.
    ///
    /// Backs `alf restore --dry-run`. Adapters that support dry-run override
    /// this; the default rejects the call so the CLI surfaces a clear error.
    fn enumerate_archive(&self, _alf_file: &Path) -> Result<ArchiveEnumeration> {
        bail!("dry-run not supported for this runtime")
    }
}
