//! Parse Hermes `memories/USER.md` into an ALF `PrincipalsDocument`.
//!
//! Hermes calls `USER.md` a *memory* target; ALF models it as the human
//! principal (Layer 3). One `Human` principal carrying the full file as prose,
//! with a best-effort structured name + timezone. Mirrors the OpenClaw/ZeroClaw
//! principals shape, but the file lives under `memories/`.

use std::collections::HashMap;
use std::fs;
use std::path::Path;

use anyhow::Result;
use uuid::Uuid;

use alf_core::{
    Principal, PrincipalProfile, PrincipalType, PrincipalsDocument, ProseProfile, StructuredProfile,
};

/// Build a `PrincipalsDocument` from `memories/USER.md` under the Hermes home.
///
/// Returns `None` if the file is missing or empty.
pub fn parse_principals(home: &Path, agent_id: Uuid) -> Result<Option<PrincipalsDocument>> {
    let user_path = home.join("memories").join("USER.md");
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

    // Deterministic ids keyed by a durable role + mtime so an unchanged USER.md
    // re-exports identically (no spurious delta). See alf_core::ids.
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

fn extract_name_field(content: &str) -> Option<String> {
    content.lines().find_map(|line| {
        let trimmed = line
            .trim()
            .strip_prefix("- ")
            .or_else(|| line.trim().strip_prefix("* "))
            .unwrap_or(line.trim())
            .trim();
        let lower = trimmed.to_ascii_lowercase();
        let rest = if lower.starts_with("**name:**") {
            &trimmed["**name:**".len()..]
        } else if lower.starts_with("**name**:") {
            &trimmed["**name**:".len()..]
        } else if lower.starts_with("name:") {
            &trimmed["name:".len()..]
        } else {
            return None;
        };
        let val = rest.trim().trim_matches('*').trim();
        (!val.is_empty()).then(|| val.to_string())
    })
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
    let mut in_tz = false;
    for line in content.lines() {
        let t = line.trim();
        if t.starts_with("## ") {
            in_tz = t.to_lowercase().contains("timezone") || t.to_lowercase().contains("time zone");
            continue;
        }
        if in_tz && !t.is_empty() {
            if t.contains('/') && !t.contains(' ') {
                return Some(t.to_string());
            }
            if let Some(tz) = t.split_whitespace().find(|w| w.contains('/')) {
                return Some(tz.to_string());
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_user_md(home: &Path, body: &str) {
        let mem = home.join("memories");
        fs::create_dir_all(&mem).unwrap();
        fs::write(mem.join("USER.md"), body).unwrap();
    }

    #[test]
    fn full_user_md() {
        let tmp = tempfile::tempdir().unwrap();
        write_user_md(
            tmp.path(),
            "# Alice\n\nSoftware engineer.\n\n## Timezone\n\nAmerica/New_York\n",
        );
        let doc = parse_principals(tmp.path(), Uuid::new_v4())
            .unwrap()
            .unwrap();
        assert_eq!(doc.principals.len(), 1);
        let p = &doc.principals[0];
        assert_eq!(p.principal_type, PrincipalType::Human);
        let s = p.profile.structured.as_ref().unwrap();
        assert_eq!(s.name.as_deref(), Some("Alice"));
        assert_eq!(s.timezone.as_deref(), Some("America/New_York"));
        assert!(p.profile.prose.as_ref().unwrap().user_profile.is_some());
    }

    #[test]
    fn name_field_beats_heading() {
        let tmp = tempfile::tempdir().unwrap();
        write_user_md(tmp.path(), "# About\n\n- **Name:** Johan\n");
        let doc = parse_principals(tmp.path(), Uuid::new_v4())
            .unwrap()
            .unwrap();
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
    fn missing_or_empty_is_none() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(parse_principals(tmp.path(), Uuid::new_v4())
            .unwrap()
            .is_none());
        write_user_md(tmp.path(), "   \n  ");
        assert!(parse_principals(tmp.path(), Uuid::new_v4())
            .unwrap()
            .is_none());
    }
}
