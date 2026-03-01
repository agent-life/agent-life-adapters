//! Adapter registry.
//!
//! Each agent framework (OpenClaw, ZeroClaw, etc.) implements the
//! [`alf_core::Adapter`] trait. The CLI dispatches to the correct adapter
//! based on the `--runtime` flag.

use anyhow::Result;
use std::path::Path;

pub use alf_core::{Adapter, ExportReport, ImportReport};

// ---------------------------------------------------------------------------
// Registry
// ---------------------------------------------------------------------------

/// Returns the list of all available adapters.
pub fn available_adapters() -> Vec<Box<dyn Adapter>> {
    vec![
        Box::new(StubOpenClawAdapter),
        Box::new(StubZeroClawAdapter),
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
// Stub ZeroClaw adapter (placeholder until adapter-zeroclaw crate wired in)
// ---------------------------------------------------------------------------

struct StubZeroClawAdapter;

impl Adapter for StubZeroClawAdapter {
    fn name(&self) -> &str {
        "zeroclaw"
    }

    fn description(&self) -> &str {
        "ZeroClaw agent framework (configurable backend workspace)"
    }

    fn export(&self, workspace: &Path, output: &Path) -> Result<ExportReport> {
        anyhow::bail!(
            "ZeroClaw adapter export is not yet wired into the CLI.\n\
             Workspace: {}\n\
             Output: {}\n\
             This will be wired in from the adapter-zeroclaw crate.",
            workspace.display(),
            output.display()
        )
    }

    fn import(&self, alf_file: &Path, workspace: &Path) -> Result<ImportReport> {
        anyhow::bail!(
            "ZeroClaw adapter import is not yet wired into the CLI.\n\
             ALF file: {}\n\
             Workspace: {}\n\
             This will be wired in from the adapter-zeroclaw crate.",
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
    fn registry_finds_zeroclaw() {
        let adapter = get_adapter("zeroclaw");
        assert!(adapter.is_some());
        assert_eq!(adapter.unwrap().name(), "zeroclaw");
    }

    #[test]
    fn registry_returns_none_for_unknown() {
        assert!(get_adapter("unknown-runtime").is_none());
    }

    #[test]
    fn supported_runtimes_includes_both() {
        let runtimes = supported_runtimes();
        assert!(runtimes.contains("openclaw"));
        assert!(runtimes.contains("zeroclaw"));
    }
}