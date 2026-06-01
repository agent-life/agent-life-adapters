//! Parse `USER.md` into an ALF `PrincipalsDocument`.
//!
//! Same approach as the OpenClaw adapter: one `Human` principal with the
//! full USER.md content as prose, structured name (`Name:` / H1 / default), and
//! optional timezone extraction.

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

/// Build a `PrincipalsDocument` from `USER.md` in the workspace.
///
/// Returns `None` if `USER.md` is missing or empty.
pub fn parse_principals(workspace: &Path, agent_id: Uuid) -> Result<Option<PrincipalsDocument>> {
    let user_path = workspace.join("USER.md");
    if !user_path.is_file() {
        return Ok(None);
    }

    let content = fs::read_to_string(&user_path)?;
    if content.trim().is_empty() {
        return Ok(None);
    }

    let name = extract_name_field(&content)
        .or_else(|| extract_h1(&content))
        .unwrap_or_else(|| "User".to_string());
    let timezone = extract_timezone(&content);

    // Deterministic ids keyed by a durable role (not the display name) + mtime,
    // so an unchanged USER.md re-exports identically (no spurious delta). See
    // alf_core::ids.
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
            updated_at: alf_core::ids::newest_mtime([&user_path]),
            structured: Some(StructuredProfile {
                name: Some(name),
                principal_type: None,
                timezone,
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
            source_format: None,
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

fn extract_h1(content: &str) -> Option<String> {
    for line in content.lines() {
        let t = line.trim();
        if t.starts_with("# ") && !t.starts_with("## ") {
            return Some(t.trim_start_matches("# ").trim().to_string());
        }
    }
    None
}

fn extract_timezone(content: &str) -> Option<String> {
    let mut in_tz_section = false;
    for line in content.lines() {
        let t = line.trim();
        if t.starts_with("## ") {
            in_tz_section =
                t.to_lowercase().contains("timezone") || t.to_lowercase().contains("time zone");
            continue;
        }
        if in_tz_section && !t.is_empty() {
            // Expect an IANA timezone string like "America/Los_Angeles"
            if t.contains('/') && !t.contains(' ') {
                return Some(t.to_string());
            }
            // Also accept lines containing the timezone
            if let Some(tz) = t.split_whitespace().find(|w| w.contains('/')) {
                return Some(tz.to_string());
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

    #[test]
    fn markdown_name_field_wins_over_heading() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path();
        fs::write(
            ws.join("USER.md"),
            "\
# USER.md - About Johan

- **Name:** Johan
- **Timezone:** PST (US West Coast)
",
        )
        .unwrap();

        let doc = parse_principals(ws, Uuid::new_v4()).unwrap().unwrap();
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
    fn full_user_md() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path();
        fs::write(
            ws.join("USER.md"),
            "# Alice\n\nSoftware engineer.\n\n## Timezone\n\nAmerica/New_York\n",
        )
        .unwrap();

        let doc = parse_principals(ws, Uuid::new_v4()).unwrap().unwrap();
        assert_eq!(doc.principals.len(), 1);
        let p = &doc.principals[0];
        assert_eq!(p.principal_type, PrincipalType::Human);
        assert_eq!(
            p.profile.structured.as_ref().unwrap().name.as_deref(),
            Some("Alice")
        );
        assert_eq!(
            p.profile.structured.as_ref().unwrap().timezone.as_deref(),
            Some("America/New_York")
        );
    }

    #[test]
    fn no_user_file() {
        let tmp = tempfile::tempdir().unwrap();
        let result = parse_principals(tmp.path(), Uuid::new_v4()).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn empty_user_file() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(tmp.path().join("USER.md"), "  \n  ").unwrap();
        let result = parse_principals(tmp.path(), Uuid::new_v4()).unwrap();
        assert!(result.is_none());
    }
}
