//! Parse OpenClaw identity files (`SOUL.md`, `IDENTITY.md`, `AGENTS.md`)
//! into an ALF `Identity`.
//!
//! The adapter stores all three files as prose blocks (lossless) and extracts
//! only the agent name as structured data. The raw files are also preserved
//! in `raw/openclaw/` for full fidelity. More structured parsing (role, goals,
//! capabilities) can be added later without breaking the format.

use std::collections::HashMap;
use std::fs;
use std::path::Path;

use anyhow::Result;
use uuid::Uuid;

use alf_core::{Identity, Names, ProseIdentity, StructuredIdentity};

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Build an `Identity` from OpenClaw workspace files.
///
/// Reads `SOUL.md`, `IDENTITY.md`, `AGENTS.md` — all optional.
/// Returns `None` if none of the three files exist.
pub fn build_identity(workspace: &Path, agent_id: Uuid) -> Result<Option<Identity>> {
    let soul_content = read_optional(workspace, "SOUL.md")?;
    let identity_content = read_optional(workspace, "IDENTITY.md")?;
    let agents_content = read_optional(workspace, "AGENTS.md")?;

    // If nothing exists, no identity to export
    if soul_content.is_none() && identity_content.is_none() && agents_content.is_none() {
        return Ok(None);
    }

    // OpenClaw's canonical display name lives in IDENTITY.md. If it is
    // missing or unparseable, fall back to the workspace folder name.
    let agent_name =
        resolve_agent_display_name_from_identity(identity_content.as_deref(), workspace);

    let prose = ProseIdentity {
        soul: soul_content,
        operating_instructions: agents_content,
        identity_profile: identity_content,
        custom_blocks: HashMap::new(),
        extra: HashMap::new(),
    };

    let structured = StructuredIdentity {
        names: Some(Names {
            primary: agent_name,
            nickname: None,
            full: None,
            extra: HashMap::new(),
        }),
        role: None,
        goals: Vec::new(),
        psychology: None,
        linguistics: None,
        capabilities: Vec::new(),
        sub_agents: Vec::new(),
        aieos_extensions: None,
        extra: HashMap::new(),
    };

    Ok(Some(Identity {
        // Deterministic id + mtime so an unchanged identity re-exports identically
        // (no spurious delta every sync). See alf_core::ids.
        id: alf_core::ids::identity_id(agent_id),
        agent_id,
        version: 1,
        updated_at: alf_core::ids::newest_mtime([
            workspace.join("SOUL.md"),
            workspace.join("IDENTITY.md"),
            workspace.join("AGENTS.md"),
        ]),
        structured: Some(structured),
        prose: Some(prose),
        source_format: Some("openclaw".to_string()),
        raw_source: None, // raw files go to raw/openclaw/ in the archive
        extra: HashMap::new(),
    }))
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Resolve the agent display name for an OpenClaw workspace.
///
/// Prefers the structured `Name` field from `IDENTITY.md`. If that field is
/// absent or cannot be parsed, falls back to the workspace directory basename.
pub fn resolve_agent_display_name(workspace: &Path) -> String {
    let identity_content = read_optional(workspace, "IDENTITY.md").ok().flatten();
    resolve_agent_display_name_from_identity(identity_content.as_deref(), workspace)
}

/// Read a file from the workspace, returning `None` if it doesn't exist.
fn read_optional(workspace: &Path, filename: &str) -> Result<Option<String>> {
    let path = workspace.join(filename);
    if path.is_file() {
        let content = fs::read_to_string(&path)?;
        if content.trim().is_empty() {
            Ok(None)
        } else {
            Ok(Some(content))
        }
    } else {
        Ok(None)
    }
}

fn resolve_agent_display_name_from_identity(
    identity_content: Option<&str>,
    workspace: &Path,
) -> String {
    identity_content
        .and_then(extract_identity_name_field)
        .unwrap_or_else(|| workspace_basename(workspace))
}

fn extract_identity_name_field(content: &str) -> Option<String> {
    content.lines().find_map(extract_identity_name_line)
}

fn extract_identity_name_line(line: &str) -> Option<String> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return None;
    }

    let without_bullet = trimmed
        .strip_prefix("- ")
        .or_else(|| trimmed.strip_prefix("* "))
        .unwrap_or(trimmed)
        .trim();
    let normalized = without_bullet.to_ascii_lowercase();

    let name = if normalized.starts_with("**name:**") {
        &without_bullet["**name:**".len()..]
    } else if normalized.starts_with("**name**:") {
        &without_bullet["**name**:".len()..]
    } else if normalized.starts_with("name:") {
        &without_bullet["name:".len()..]
    } else {
        return None;
    };

    normalize_name_value(name)
}

fn normalize_name_value(name: &str) -> Option<String> {
    let trimmed = name.trim().trim_matches('*').trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

fn workspace_basename(workspace: &Path) -> String {
    workspace
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.trim().is_empty())
        .unwrap_or("unknown")
        .to_string()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;
    use tempfile::TempDir;

    fn setup_named_workspace(name: &str, files: &[(&str, &str)]) -> (TempDir, PathBuf) {
        let dir = TempDir::new().unwrap();
        let workspace = dir.path().join(name);
        fs::create_dir_all(&workspace).unwrap();
        for (name, content) in files {
            let path = workspace.join(name);
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).unwrap();
            }
            fs::write(&path, content).unwrap();
        }
        (dir, workspace)
    }

    fn setup_workspace(files: &[(&str, &str)]) -> (TempDir, PathBuf) {
        setup_named_workspace("workspace", files)
    }

    #[test]
    fn parses_markdown_name_bullet() {
        let content = "# IDENTITY.md - Who Am I?\n\n- **Name:** Kleo\n- **Vibe:** Direct";
        assert_eq!(
            extract_identity_name_field(content).as_deref(),
            Some("Kleo")
        );
    }

    #[test]
    fn parses_plain_name_line() {
        let content = "# Identity\nRole: Assistant\nName: Standard Agent";
        assert_eq!(
            extract_identity_name_field(content).as_deref(),
            Some("Standard Agent")
        );
    }

    #[test]
    fn soul_only_falls_back_to_workspace_basename() {
        let (_dir, workspace) = setup_named_workspace(
            "clawd-workspace",
            &[("SOUL.md", "# Clawd\n\nA helpful assistant.")],
        );
        let id = build_identity(&workspace, Uuid::nil()).unwrap().unwrap();
        assert_eq!(
            id.structured
                .as_ref()
                .unwrap()
                .names
                .as_ref()
                .unwrap()
                .primary,
            "clawd-workspace"
        );
        assert!(id
            .prose
            .as_ref()
            .unwrap()
            .soul
            .as_ref()
            .unwrap()
            .contains("helpful assistant"));
        assert!(id.prose.as_ref().unwrap().operating_instructions.is_none());
    }

    #[test]
    fn identity_name_field_wins_over_soul_heading() {
        let (_dir, workspace) = setup_workspace(&[
            ("SOUL.md", "# Samantha\n\nPersonality here."),
            (
                "IDENTITY.md",
                "# Identity\n\n- **Name:** Kleo\n\n## Role\nAssistant",
            ),
            ("AGENTS.md", "# Instructions\n\nDo good things."),
        ]);
        let id = build_identity(&workspace, Uuid::nil()).unwrap().unwrap();
        assert_eq!(
            id.structured
                .as_ref()
                .unwrap()
                .names
                .as_ref()
                .unwrap()
                .primary,
            "Kleo"
        );
        assert!(id.prose.as_ref().unwrap().soul.is_some());
        assert!(id.prose.as_ref().unwrap().identity_profile.is_some());
        assert!(id.prose.as_ref().unwrap().operating_instructions.is_some());
    }

    #[test]
    fn no_files_returns_none() {
        let (_dir, workspace) = setup_workspace(&[]);
        let result = build_identity(&workspace, Uuid::nil()).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn unparseable_identity_falls_back_to_workspace_basename() {
        let (_dir, workspace) = setup_named_workspace(
            "fallback-agent",
            &[(
                "IDENTITY.md",
                "# Identity\n\nStructured identity without a name line.",
            )],
        );
        let id = build_identity(&workspace, Uuid::nil()).unwrap().unwrap();
        assert_eq!(
            id.structured
                .as_ref()
                .unwrap()
                .names
                .as_ref()
                .unwrap()
                .primary,
            "fallback-agent"
        );
    }

    #[test]
    fn identity_md_name_line_provides_name() {
        let (_dir, workspace) = setup_workspace(&[
            ("SOUL.md", "Personality text only."),
            (
                "IDENTITY.md",
                "# Identity\n\nName: WorkBot\n\nStructured identity.",
            ),
        ]);
        let id = build_identity(&workspace, Uuid::nil()).unwrap().unwrap();
        assert_eq!(
            id.structured
                .as_ref()
                .unwrap()
                .names
                .as_ref()
                .unwrap()
                .primary,
            "WorkBot"
        );
    }

    #[test]
    fn empty_file_treated_as_absent() {
        let (_dir, workspace) = setup_workspace(&[("SOUL.md", "   \n  \n")]);
        let result = build_identity(&workspace, Uuid::nil()).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn resolve_agent_display_name_uses_workspace_basename_without_identity_name() {
        let (_dir, workspace) = setup_named_workspace(
            "basename-fallback",
            &[("SOUL.md", "# Template\n\nPersona text.")],
        );
        assert_eq!(resolve_agent_display_name(&workspace), "basename-fallback");
    }
}
