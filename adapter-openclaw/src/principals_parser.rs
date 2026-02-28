//! Parse OpenClaw `USER.md` into an ALF `PrincipalsDocument`.
//!
//! The USER.md file contains the human principal's profile — name, preferences,
//! work context, timezone. The adapter stores the full Markdown as prose and
//! extracts only the name as structured data.

use std::collections::HashMap;
use std::fs;
use std::path::Path;

use anyhow::Result;
use chrono::Utc;
use uuid::Uuid;

use alf_core::{
    Principal, PrincipalProfile, PrincipalType, PrincipalsDocument, ProseProfile,
    StructuredProfile,
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

    let principal_name = extract_h1_heading(&content).unwrap_or_else(|| "User".to_string());
    let principal_id = Uuid::new_v7(uuid::Timestamp::now(uuid::NoContext));
    let profile_id = Uuid::new_v7(uuid::Timestamp::now(uuid::NoContext));

    let principal = Principal {
        id: principal_id,
        principal_type: PrincipalType::Human,
        agent_id: None,
        profile: PrincipalProfile {
            id: profile_id,
            agent_id,
            principal_id,
            version: 1,
            updated_at: Utc::now(),
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