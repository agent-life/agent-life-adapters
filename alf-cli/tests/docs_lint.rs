//! RF-023 docs regression lint: the published docs must not regress to the
//! stale claims the 2026-07-30 release review found (old vault/key paths
//! presented as current, obsolete version strings, runtime lists missing
//! `generic`, and the false SQLite/auto-walk absolute claims).
//!
//! Scope: the ACTIVE docs only. `CHANGELOG.md` is linted for its top
//! (unreleased) section alone — historical release notes are allowlisted and
//! must not be rewritten to satisfy a lint (RF-023 scope item 5).

use std::fs;
use std::path::{Path, PathBuf};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .to_path_buf()
}

fn read(rel: &str) -> String {
    let path = repo_root().join(rel);
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()))
}

/// The docs that must describe CURRENT behavior.
const ACTIVE_DOCS: &[&str] = &[
    "README.md",
    "docs/cli-reference.md",
    "docs/vault-key-management.md",
    "docs/alf-mcp-server-user-manual.md",
    "skills/agent-life/SKILL.md",
];

/// Phrases that are false for 1.1 and must not reappear anywhere in an active doc.
const FORBIDDEN_PHRASES: &[&str] = &[
    // WP5 shipped a hermes default key path (vault_key.rs "hermes" arm).
    "hermes has no default key path",
    "hermes keeps no default key path",
    // The v1 raw SQLite capture is a quiesced near-consistent byte copy — no
    // transactional guarantee (watch/capture.rs module doc).
    "captured together as one consistent unit",
    // OpenClaw scatter-captures every workspace .md — absolute no-walk claims
    // are false (adapter-openclaw/src/export.rs scatter capture).
    "never auto-walks",
    "auto-walking or slurping",
    "ALF never auto-walks",
    // Stale three-runtime lists (the registry has four runtimes).
    "`zeroclaw`, or `hermes` |",
    "`zeroclaw`, and `hermes`",
    "zeroclaw, and hermes.",
    "(openclaw, zeroclaw, hermes).",
    "(openclaw, zeroclaw).",
];

/// Legacy paths may be MENTIONED, but only on lines that mark them as legacy.
const LEGACY_MARKERS: &[&str] = &[
    "legacy",
    "Legacy",
    "mapping-less",
    "migrat", // migrate / migrated / migration
    "pre-multi-agent",
    "pre-WP1",
];

fn line_marks_legacy(line: &str) -> bool {
    LEGACY_MARKERS.iter().any(|m| line.contains(m))
}

#[test]
fn active_docs_carry_no_stale_claims() {
    let mut violations = Vec::new();
    for doc in ACTIVE_DOCS {
        let text = read(doc);
        for phrase in FORBIDDEN_PHRASES {
            for (i, line) in text.lines().enumerate() {
                if line.contains(phrase) {
                    violations.push(format!("{doc}:{}: forbidden phrase {phrase:?}", i + 1));
                }
            }
        }
        // Legacy install-scoped paths must be labeled as legacy on the same line.
        for (i, line) in text.lines().enumerate() {
            let legacy_vault = line.contains(".alf/vault/credentials.json");
            let legacy_key = line.contains("state/.alf-vault-key")
                && !line.contains("<alf-agent-id>/.alf-vault-key")
                && !line.contains("{alf_agent_id}/.alf-vault-key")
                && !line.contains("<alf_agent_id>/.alf-vault-key");
            if (legacy_vault || legacy_key) && !line_marks_legacy(line) {
                violations.push(format!(
                    "{doc}:{}: legacy install-scoped path stated without a legacy marker: {}",
                    i + 1,
                    line.trim()
                ));
            }
        }
    }
    assert!(
        violations.is_empty(),
        "stale-doc lint failed (RF-023):\n{}",
        violations.join("\n")
    );
}

#[test]
fn cli_reference_version_matches_crate() {
    let text = read("docs/cli-reference.md");
    let expected = format!("> Version: {} |", env!("CARGO_PKG_VERSION"));
    assert!(
        text.contains(&expected),
        "docs/cli-reference.md header must state the current CLI version \
         ({expected:?} not found) — update the doc when bumping alf-cli"
    );
    // The install example must not regress to an obsolete version string.
    for stale in ["\"version\":\"v0.2", "alf 0.2.0", "Version: 1.0.0 |"] {
        assert!(
            !text.contains(stale),
            "docs/cli-reference.md contains obsolete version string {stale:?}"
        );
    }
}

/// Every top-level section the published reference (and the web TOC that
/// mirrors it) must expose, exactly once.
#[test]
fn cli_reference_sections_present_exactly_once() {
    let text = read("docs/cli-reference.md");
    let expected = [
        "## Global Flags",
        "## Environment variables",
        "## Runtime and workspace defaults",
        "## Quick Reference",
        "## alf check",
        "## alf login",
        "## alf export",
        "## alf add",
        "## alf sync",
        "## alf restore",
        "## alf agents",
        "## alf purge",
        "## alf import",
        "## alf validate",
        "## alf vault",
        "## alf help",
        "## alf mcp serve",
        "## MCP client configuration",
        "## The generic runtime map file (`.alf-map.json`)",
        "## Decommissioning an agent",
        "## Error JSON",
        "## Configuration",
        "## Install",
        "## File Layout",
    ];
    for heading in expected {
        let needle = format!("\n{heading}\n");
        let count = text.matches(&needle).count();
        assert_eq!(
            count, 1,
            "docs/cli-reference.md: heading {heading:?} found {count} times (expected exactly 1)"
        );
    }
}

/// The top (unreleased/current) CHANGELOG section only — history is allowlisted.
#[test]
fn changelog_current_section_is_clean() {
    let text = read("CHANGELOG.md");
    let current: String = {
        let mut sections = text.split("\n## [");
        sections.next().unwrap_or("").to_string()
    };
    for phrase in ["hermes has no default key path", "never auto-walks"] {
        assert!(
            !current.contains(phrase),
            "CHANGELOG.md current section contains stale claim {phrase:?}"
        );
    }
}
