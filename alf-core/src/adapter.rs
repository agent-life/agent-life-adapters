//! Adapter trait and report types shared across all framework adapters.
//!
//! Each agent framework (OpenClaw, ZeroClaw, etc.) implements the [`Adapter`]
//! trait. The CLI dispatches to the correct adapter based on the `--runtime`
//! flag.

use anyhow::Result;
use std::path::Path;

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
    /// `workspace` is the path to the framework's workspace directory.
    /// `output` is the path to write the .alf file.
    fn export(&self, workspace: &Path, output: &Path) -> Result<ExportReport>;

    /// Import an .alf file into a workspace.
    ///
    /// `alf_file` is the path to the .alf archive.
    /// `workspace` is the target workspace directory (created if it doesn't exist).
    fn import(&self, alf_file: &Path, workspace: &Path) -> Result<ImportReport>;
}