//! Parse OpenClaw `USER.md` into an ALF `PrincipalsDocument`.
//!
//! The USER.md file contains the human principal's profile — name, preferences,
//! work context, timezone. The adapter stores the full Markdown as prose and
//! extracts only the name as structured data.

use std::collections::HashMap;
use std::fs;
use std::path::Path;

use anyhow::Result;
use uuid::Uuid;

use alf_core::{
    Principal, PrincipalProfile, PrincipalType, PrincipalsDocument, ProseProfile, StructuredProfile,
};

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Build a `PrincipalsDocument` from `USER.md`.
///
/// Returns `None` if `USER.md` doesn't exist or is empty.
pub fn build_principals(workspace: &Path, agent_id: Uuid) -> Result<Option<PrincipalsDocument>> {
    let path = workspace.join("USER.md");
    if !path.is_file() {
        return Ok(None);
    }

    let content = fs::read_to_string(&path)?;
    if content.trim().is_empty() {
        return Ok(None);
    }

    let principal_name = extract_name_field(&content)
        .or_else(|| extract_h1_heading(&content))
        .unwrap_or_else(|| "User".to_string());
    // Deterministic ids keyed by a durable role (not the display name) + mtime,
    // so an unchanged USER.md re-exports identically (no spurious delta). The
    // single OpenClaw human principal uses the "human" role. See alf_core::ids.
    let principal_id = alf_core::ids::principal_id(agent_id, "human");
    let profile_id = alf_core::ids::profile_id(principal_id);

    let principal = Principal {
        id: principal_id,
        principal_type: PrincipalType::Human,
        agent_id: None,
        profile: PrincipalProfile {
            id: profile_id,
            agent_id,
            principal_id,
            version: 1,
            updated_at: alf_core::ids::newest_mtime([&path]),
            structured: Some(StructuredProfile {
                name: Some(principal_name),
                principal_type: Some("human".to_string()),
                timezone: extract_timezone(&content),
                locale: None,
                communication_preferences: None,
                work_context: None,
                relationships: Vec::new(),
                custom_fields: None,
                extra: HashMap::new(),
            }),
            prose: Some(ProseProfile {
                user_profile: Some(content),
                extra: HashMap::new(),
            }),
            source_format: Some("openclaw".to_string()),
            raw_source: None,
            extra: HashMap::new(),
        },
        extra: HashMap::new(),
    };

    Ok(Some(PrincipalsDocument {
        principals: vec![principal],
        extra: HashMap::new(),
    }))
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Extract the text of the first `# ` (H1) heading.
fn extract_h1_heading(content: &str) -> Option<String> {
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("# ") && !trimmed.starts_with("## ") {
            return Some(trimmed.trim_start_matches("# ").trim().to_string());
        }
    }
    None
}

/// Extract the user's name from a structured `Name` line in `USER.md`.
fn extract_name_field(content: &str) -> Option<String> {
    content.lines().find_map(extract_name_line)
}

fn extract_name_line(line: &str) -> Option<String> {
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

/// Try to extract a timezone from a `## Timezone` section in USER.md.
/// Looks for a line under `## Timezone` that resembles an IANA timezone.
fn extract_timezone(content: &str) -> Option<String> {
    let mut in_timezone_section = false;
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("## ") {
            in_timezone_section = trimmed
                .trim_start_matches("## ")
                .trim()
                .eq_ignore_ascii_case("timezone");
            continue;
        }
        if in_timezone_section && !trimmed.is_empty() {
            // Accept lines that look like IANA timezones (e.g., "America/Los_Angeles")
            if trimmed.contains('/') && !trimmed.starts_with('-') && !trimmed.starts_with('#') {
                return Some(trimmed.to_string());
            }
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

    #[test]
    fn markdown_name_field_wins_over_heading() {
        let dir = TempDir::new().unwrap();
        fs::write(
            dir.path().join("USER.md"),
            "\
# USER.md - About Johan

- **Name:** Johan
- **Timezone:** PST (US West Coast)
",
        )
        .unwrap();

        let doc = build_principals(dir.path(), Uuid::nil()).unwrap().unwrap();
        assert_eq!(
            doc.principals[0]
                .profile
                .structured
                .as_ref()
                .unwrap()
                .name
                .as_deref(),
            Some("Johan")
        );
    }

    #[test]
    fn plain_name_field_wins_over_heading() {
        let dir = TempDir::new().unwrap();
        fs::write(
            dir.path().join("USER.md"),
            "\
# User Profile
Name: Human User
Timezone: UTC
",
        )
        .unwrap();

        let doc = build_principals(dir.path(), Uuid::nil()).unwrap().unwrap();
        assert_eq!(
            doc.principals[0]
                .profile
                .structured
                .as_ref()
                .unwrap()
                .name
                .as_deref(),
            Some("Human User")
        );
    }

    #[test]
    fn full_user_md() {
        let dir = TempDir::new().unwrap();
        fs::write(
            dir.path().join("USER.md"),
            "\
# Alice

## Preferences

- tone: casual
- response_length: concise

## Timezone

America/Los_Angeles
",
        )
        .unwrap();

        let doc = build_principals(dir.path(), Uuid::nil()).unwrap().unwrap();
        assert_eq!(doc.principals.len(), 1);
        let p = &doc.principals[0];
        assert_eq!(p.principal_type, PrincipalType::Human);
        assert_eq!(
            p.profile.structured.as_ref().unwrap().name.as_deref(),
            Some("Alice")
        );
        assert_eq!(
            p.profile.structured.as_ref().unwrap().timezone.as_deref(),
            Some("America/Los_Angeles")
        );
        assert!(p
            .profile
            .prose
            .as_ref()
            .unwrap()
            .user_profile
            .as_ref()
            .unwrap()
            .contains("casual"));
    }

    #[test]
    fn no_user_md_returns_none() {
        let dir = TempDir::new().unwrap();
        let result = build_principals(dir.path(), Uuid::nil()).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn empty_user_md_returns_none() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("USER.md"), "   \n").unwrap();
        let result = build_principals(dir.path(), Uuid::nil()).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn no_heading_defaults_to_user() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("USER.md"), "Just some profile text.").unwrap();
        let doc = build_principals(dir.path(), Uuid::nil()).unwrap().unwrap();
        assert_eq!(
            doc.principals[0]
                .profile
                .structured
                .as_ref()
                .unwrap()
                .name
                .as_deref(),
            Some("User")
        );
    }
}
