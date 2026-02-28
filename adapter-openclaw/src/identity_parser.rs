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
use chrono::Utc;
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

    // Extract agent name from the first H1 heading in SOUL.md, then
    // IDENTITY.md, then fall back to "Unknown".
    let agent_name = soul_content
        .as_deref()
        .and_then(extract_h1_heading)
        .or_else(|| identity_content.as_deref().and_then(extract_h1_heading))
        .unwrap_or_else(|| "Unknown".to_string());

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
        id: Uuid::new_v7(uuid::Timestamp::now(uuid::NoContext)),
        agent_id,
        version: 1,
        updated_at: Utc::now(),
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

/// Extract the text of the first `# ` (H1) heading in Markdown content.
fn extract_h1_heading(content: &str) -> Option<String> {
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("# ") && !trimmed.starts_with("## ") {
            return Some(trimmed.trim_start_matches("# ").trim().to_string());
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn setup_workspace(files: &[(&str, &str)]) -> TempDir {
        let dir = TempDir::new().unwrap();
        for (name, content) in files {
            let path = dir.path().join(name);
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).unwrap();
            }
            fs::write(&path, content).unwrap();
        }
        dir
    }

    #[test]
    fn soul_only() {
        let ws = setup_workspace(&[("SOUL.md", "# Clawd\n\nA helpful assistant.")]);
        let id = build_identity(ws.path(), Uuid::nil()).unwrap().unwrap();
        assert_eq!(id.structured.as_ref().unwrap().names.as_ref().unwrap().primary, "Clawd");
        assert!(id.prose.as_ref().unwrap().soul.as_ref().unwrap().contains("helpful assistant"));
        assert!(id.prose.as_ref().unwrap().operating_instructions.is_none());
    }

    #[test]
    fn all_three_files() {
        let ws = setup_workspace(&[
            ("SOUL.md", "# Samantha\n\nPersonality here."),
            ("IDENTITY.md", "# Identity\n\n## Role\nAssistant"),
            ("AGENTS.md", "# Instructions\n\nDo good things."),
        ]);
        let id = build_identity(ws.path(), Uuid::nil()).unwrap().unwrap();
        // Name comes from SOUL.md (priority)
        assert_eq!(id.structured.as_ref().unwrap().names.as_ref().unwrap().primary, "Samantha");
        assert!(id.prose.as_ref().unwrap().soul.is_some());
        assert!(id.prose.as_ref().unwrap().identity_profile.is_some());
        assert!(id.prose.as_ref().unwrap().operating_instructions.is_some());
    }

    #[test]
    fn no_files_returns_none() {
        let ws = TempDir::new().unwrap();
        let result = build_identity(ws.path(), Uuid::nil()).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn no_h1_heading_falls_back_to_unknown() {
        let ws = setup_workspace(&[("SOUL.md", "Just some text without a heading.")]);
        let id = build_identity(ws.path(), Uuid::nil()).unwrap().unwrap();
        assert_eq!(id.structured.as_ref().unwrap().names.as_ref().unwrap().primary, "Unknown");
    }

    #[test]
    fn identity_md_provides_name_when_soul_has_none() {
        let ws = setup_workspace(&[
            ("SOUL.md", "Personality text only."),
            ("IDENTITY.md", "# WorkBot\n\nStructured identity."),
        ]);
        let id = build_identity(ws.path(), Uuid::nil()).unwrap().unwrap();
        assert_eq!(id.structured.as_ref().unwrap().names.as_ref().unwrap().primary, "WorkBot");
    }

    #[test]
    fn empty_file_treated_as_absent() {
        let ws = setup_workspace(&[("SOUL.md", "   \n  \n")]);
        let result = build_identity(ws.path(), Uuid::nil()).unwrap();
        assert!(result.is_none());
    }
}