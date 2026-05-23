//! Shared test helpers for the OpenClaw adapter integration suites.

use std::sync::OnceLock;
use tempfile::TempDir;

/// Point `HOME` at a clean temp dir for the whole test process, set exactly
/// once before any vault access. `import()` writes credentials to
/// `$HOME/.alf/vault` and auth profiles to `$HOME/.openclaw/...`, and
/// `export()` reads `$HOME/.alf/vault` — without this, import tests would read
/// or rewrite the developer's real vault. Call at the start of any test that
/// imports (the export within it is then isolated too).
pub fn isolate_home() {
    static TEST_HOME: OnceLock<TempDir> = OnceLock::new();
    TEST_HOME.get_or_init(|| {
        let home = TempDir::new().unwrap();
        std::env::set_var("HOME", home.path());
        home
    });
}
