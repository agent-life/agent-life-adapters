//! Adapter registry.
//!
//! Each agent framework (OpenClaw, ZeroClaw, etc.) implements the
//! [`alf_core::Adapter`] trait. The CLI dispatches to the correct adapter
//! based on the `--runtime` flag.

pub use alf_core::Adapter;

// ---------------------------------------------------------------------------
// Registry
// ---------------------------------------------------------------------------

/// Returns the list of all available adapters.
pub fn available_adapters() -> Vec<Box<dyn Adapter>> {
    vec![
        Box::new(adapter_openclaw::OpenClawAdapter),
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