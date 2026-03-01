//! Parse `USER.md` into an ALF `PrincipalsDocument`.
//!
//! Same approach as the OpenClaw adapter: one `Human` principal with the
//! full USER.md content as prose and optional timezone extraction.

use std::collections::HashMap;
use std::fs;
use std::path::Path;

use anyhow::Result;
use chrono::Utc;
use uuid::Uuid;

use alf_core::{
    Principal, PrincipalProfile, PrincipalType, PrincipalsDocument, StructuredProfile,
    ProseProfile,
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

    let name = extract_h1(&content).unwrap_or_else(|| "User".to_string());
    let timezone = extract_timezone(&content);

    let principal_id = Uuid::new_v4();
    let profile_id = Uuid::new_v4();

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
            in_tz_section = t.to_lowercase().contains("timezone")
                || t.to_lowercase().contains("time zone");
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
    fn full_user_md() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path();
        fs::write(
            ws.join("USER.md"),
            "# Alice\n\nSoftware engineer.\n\n## Timezone\n\nAmerica/New_York\n",
        ).unwrap();

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