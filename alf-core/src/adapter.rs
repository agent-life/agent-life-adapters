//! Adapter trait and report types shared across all framework adapters.
//!
//! Each agent framework (OpenClaw, ZeroClaw, etc.) implements the [`Adapter`]
//! trait. The CLI dispatches to the correct adapter based on the `--runtime`
//! flag.

use anyhow::Result;
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
// Export / Import options
// ---------------------------------------------------------------------------

/// Optional inputs for an export run.
///
/// `vault_key`, when supplied, tells the adapter to read real credential
/// material from the runtime and emit AEAD ciphertext in
/// `CredentialRecord.encrypted_payload`. When absent, adapters fall back
/// to the legacy metadata-only path (`<not-exported>` placeholder).
#[derive(Default)]
pub struct ExportOptions<'a> {
    pub vault_key: Option<&'a VaultKey>,
}

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

    /// Export a workspace to an .alf file with no options.
    ///
    /// Default implementation delegates to
    /// [`export_with_options`](Self::export_with_options) so existing
    /// callers don't break, while new callers can pass a vault key.
    fn export(&self, workspace: &Path, output: &Path) -> Result<ExportReport> {
        self.export_with_options(workspace, output, ExportOptions::default())
    }

    /// Export a workspace to an .alf file with caller-supplied options.
    ///
    /// `workspace` is the path to the framework's workspace directory.
    /// `output` is the path to write the .alf file.
    fn export_with_options(
        &self,
        workspace: &Path,
        output: &Path,
        options: ExportOptions<'_>,
    ) -> Result<ExportReport>;

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
}
