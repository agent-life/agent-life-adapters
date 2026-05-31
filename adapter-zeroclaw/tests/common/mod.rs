//! Shared test helpers for the ZeroClaw adapter integration suites.
//!
//! Each integration test is its own crate, so a helper used by some suites but
//! not others reads as dead code in the suites that skip it. Allow it here.
#![allow(dead_code)]

use std::fs;
use std::path::Path;
use std::sync::OnceLock;
use tempfile::TempDir;

/// Point `HOME` at a clean temp dir for the whole test process, set exactly
/// once before any vault access. `import()` writes credentials to
/// `$HOME/.alf/vault`, and `export()` reads `$HOME/.alf/vault` — without this,
/// import tests would read or rewrite the developer's real vault. Mirrors the
/// OpenClaw adapter's `common::isolate_home`.
pub fn isolate_home() {
    static TEST_HOME: OnceLock<TempDir> = OnceLock::new();
    TEST_HOME.get_or_init(|| {
        let home = TempDir::new().unwrap();
        std::env::set_var("HOME", home.path());
        std::env::remove_var("ALF_HOME");
        home
    });
}

/// Build a minimal Markdown-backend ZeroClaw home under `root`:
///
/// ```text
/// root/
///   config.toml            (backend = "markdown")
///   workspace/
///     SOUL.md IDENTITY.md AGENTS.md USER.md TOOLS.md HEARTBEAT.md
///     memory/2026-01-15.md
/// ```
///
/// Returns the workspace path (`root/workspace`) — the `-w` argument the
/// adapter's `export`/`import` expect. The Markdown backend keeps the fixture
/// hermetic (no SQLite seeding required) while still exercising the memory
/// layer.
pub fn make_markdown_home(root: &Path) -> std::path::PathBuf {
    let workspace = root.join("workspace");
    fs::create_dir_all(workspace.join("memory")).unwrap();

    fs::write(
        root.join("config.toml"),
        "schema_version = 3\n\n[memory]\nbackend = \"markdown\"\nauto_save = true\n\n[identity]\nformat = \"openclaw\"\n\n[secrets]\nencrypt = false\n",
    )
    .unwrap();

    fs::write(workspace.join("SOUL.md"), "# Aria\n\n_assistant_\n").unwrap();
    fs::write(
        workspace.join("IDENTITY.md"),
        "# IDENTITY.md\n\n- **Name:** Aria\n- **Runtime:** zeroclaw\n",
    )
    .unwrap();
    fs::write(workspace.join("AGENTS.md"), "# AGENTS.md\n\nWorkspace home.\n").unwrap();
    fs::write(
        workspace.join("USER.md"),
        "# USER.md - About Sam\n\n- **Name:** Sam\n",
    )
    .unwrap();
    fs::write(workspace.join("TOOLS.md"), "# TOOLS.md\n\nEmail via himalaya.\n").unwrap();
    fs::write(workspace.join("HEARTBEAT.md"), "# HEARTBEAT.md\n\nCheck markers.\n").unwrap();
    fs::write(
        workspace.join("memory/2026-01-15.md"),
        "# 2026-01-15\n\nSam prefers concise replies.\n",
    )
    .unwrap();

    workspace
}
