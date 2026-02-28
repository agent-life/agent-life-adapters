//! Adapter trait and runtime registry.
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
    #[allow(dead_code)]
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

// ---------------------------------------------------------------------------
// Registry
// ---------------------------------------------------------------------------

/// Returns the list of all available adapters.
pub fn available_adapters() -> Vec<Box<dyn Adapter>> {
    vec![
        Box::new(StubOpenClawAdapter),
        // Box::new(StubZeroClawAdapter), // coming later
    ]
}

/// Look up an adapter by runtime name.
pub fn get_adapter(runtime: &str) -> Option<Box<dyn Adapter>> {
    available_adapters()
        .into_iter()
        .find(|a| a.name() == runtime)
}

/// Returns a comma-separated list of supported runtime names.
pub fn supported_runtimes() -> String {
    available_adapters()
        .iter()
        .map(|a| a.name().to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

// ---------------------------------------------------------------------------
// Stub OpenClaw adapter (placeholder until adapter-openclaw crate)
// ---------------------------------------------------------------------------

struct StubOpenClawAdapter;

impl Adapter for StubOpenClawAdapter {
    fn name(&self) -> &str {
        "openclaw"
    }

    fn description(&self) -> &str {
        "OpenClaw agent framework (file-based workspace)"
    }

    fn export(&self, workspace: &Path, output: &Path) -> Result<ExportReport> {
        anyhow::bail!(
            "OpenClaw adapter export is not yet implemented.\n\
             Workspace: {}\n\
             Output: {}\n\
             This will be implemented in the adapter-openclaw crate.",
            workspace.display(),
            output.display()
        )
    }

    fn import(&self, alf_file: &Path, workspace: &Path) -> Result<ImportReport> {
        anyhow::bail!(
            "OpenClaw adapter import is not yet implemented.\n\
             ALF file: {}\n\
             Workspace: {}\n\
             This will be implemented in the adapter-openclaw crate.",
            alf_file.display(),
            workspace.display()
        )
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_finds_openclaw() {
        let adapter = get_adapter("openclaw");
        assert!(adapter.is_some());
        assert_eq!(adapter.unwrap().name(), "openclaw");
    }

    #[test]
    fn registry_returns_none_for_unknown() {
        assert!(get_adapter("unknown-runtime").is_none());
    }

    #[test]
    fn supported_runtimes_includes_openclaw() {
        let runtimes = supported_runtimes();
        assert!(runtimes.contains("openclaw"));
    }
}