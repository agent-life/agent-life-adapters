//! Principal layer types for the ALF format.
//!
//! Matches `principals.schema.json`. Principals are entities the agent takes
//! direction from — human users or managing agents. See §3.3 of the ALF
//! specification.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use uuid::Uuid;

// forward_compatible_enum! is available via #[macro_use] on mod memory in lib.rs

// ---------------------------------------------------------------------------
// Enums
// ---------------------------------------------------------------------------

forward_compatible_enum! {
    /// Type of principal (§3.3.1).
    ///
    /// Unknown values are treated as `Human` for processing purposes.
    pub enum PrincipalType {
        Human => "human",
        Agent => "agent",
    }
}

// ---------------------------------------------------------------------------
// Top-level container
// ---------------------------------------------------------------------------

/// Layer 3: Principals and user context (§3.3).
///
/// This is the top-level JSON object stored in the principals file
/// referenced by the manifest.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PrincipalsDocument {
    /// List of principals.
    pub principals: Vec<Principal>,

    /// Unknown fields preserved for forward compatibility.
    #[serde(flatten)]
    pub extra: HashMap<String, serde_json::Value>,
}

// ---------------------------------------------------------------------------
// Principal
// ---------------------------------------------------------------------------

/// An entity the agent takes direction from (§3.3.1).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Principal {
    /// Unique identifier for this principal relationship.
    pub id: Uuid,

    /// Type of principal.
    pub principal_type: PrincipalType,

    /// For agent-type principals, the managing agent's ID.
    /// `None` for human principals.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<Uuid>,

    /// Profile and preferences for this principal.
    pub profile: PrincipalProfile,

    /// Unknown fields preserved for forward compatibility.
    #[serde(flatten)]
    pub extra: HashMap<String, serde_json::Value>,
}

// ---------------------------------------------------------------------------
// Profile
// ---------------------------------------------------------------------------

/// Profile and preferences for a principal (§3.3.3).
///
/// Versioned independently from the identity layer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PrincipalProfile {
    /// Unique identifier for this profile document.
    pub id: Uuid,

    /// The agent this profile belongs to.
    pub agent_id: Uuid,

    /// The principal this profile describes.
    pub principal_id: Uuid,

    /// Profile version number.
    pub version: u32,

    /// When this version was created.
    pub updated_at: DateTime<Utc>,

    /// Machine-readable profile fields.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub structured: Option<StructuredProfile>,

    /// Rich text profile blocks.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prose: Option<ProseProfile>,

    /// The runtime that originally defined this profile.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_format: Option<String>,

    /// Original runtime-specific profile for lossless round-trip.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub raw_source: Option<serde_json::Value>,

    /// Unknown fields preserved for forward compatibility.
    #[serde(flatten)]
    pub extra: HashMap<String, serde_json::Value>,
}

// ---------------------------------------------------------------------------
// Structured Profile
// ---------------------------------------------------------------------------

/// Machine-readable principal profile fields.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StructuredProfile {
    /// The principal's name.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,

    /// Echoed from the parent principal for convenience.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub principal_type: Option<String>,

    /// IANA timezone identifier (e.g., `"America/Los_Angeles"`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timezone: Option<String>,

    /// BCP-47 locale tag (e.g., `"en-US"`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub locale: Option<String>,

    /// How the principal prefers to interact.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub communication_preferences: Option<CommunicationPreferences>,

    /// Professional context.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub work_context: Option<WorkContext>,

    /// Named relationships relevant to this principal.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub relationships: Vec<serde_json::Value>,

    /// Runtime-specific or user-defined fields for round-trip fidelity.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub custom_fields: Option<serde_json::Value>,

    /// Unknown fields preserved for forward compatibility.
    #[serde(flatten)]
    pub extra: HashMap<String, serde_json::Value>,
}

/// How a principal prefers to interact with the agent.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CommunicationPreferences {
    /// Preferred communication tone (e.g., `"casual"`, `"formal"`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tone: Option<String>,

    /// Preferred response length (e.g., `"concise"`, `"detailed"`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response_length: Option<String>,

    /// Preferred formatting style.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub formatting: Option<String>,

    /// Unknown fields preserved for forward compatibility.
    #[serde(flatten)]
    pub extra: HashMap<String, serde_json::Value>,
}

/// Professional context for a principal.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkContext {
    /// The principal's role/title.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,

    /// The principal's company/organization.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub company: Option<String>,

    /// Active projects.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub projects: Vec<String>,

    /// Unknown fields preserved for forward compatibility.
    #[serde(flatten)]
    pub extra: HashMap<String, serde_json::Value>,
}

// ---------------------------------------------------------------------------
// Prose Profile
// ---------------------------------------------------------------------------

/// Rich text profile blocks interpreted by the LLM at runtime.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProseProfile {
    /// Full content of USER.md or equivalent.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_profile: Option<String>,

    /// Unknown fields preserved for forward compatibility.
    #[serde(flatten)]
    pub extra: HashMap<String, serde_json::Value>,
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use pretty_assertions::assert_eq;

    fn sample_principals_document() -> PrincipalsDocument {
        let now = Utc.with_ymd_and_hms(2026, 2, 15, 12, 0, 0).unwrap();
        let agent_id = Uuid::new_v4();
        let principal_id = Uuid::new_v4();
        let profile_id = Uuid::new_v4();

        PrincipalsDocument {
            principals: vec![Principal {
                id: principal_id,
                principal_type: PrincipalType::Human,
                agent_id: None,
                profile: PrincipalProfile {
                    id: profile_id,
                    agent_id,
                    principal_id,
                    version: 2,
                    updated_at: now,
                    structured: Some(StructuredProfile {
                        name: Some("Alice".into()),
                        principal_type: Some("human".into()),
                        timezone: Some("America/Los_Angeles".into()),
                        locale: Some("en-US".into()),
                        communication_preferences: Some(CommunicationPreferences {
                            tone: Some("casual".into()),
                            response_length: Some("concise".into()),
                            formatting: Some("minimal markdown".into()),
                            extra: HashMap::new(),
                        }),
                        work_context: Some(WorkContext {
                            role: Some("Software Engineer".into()),
                            company: Some("Acme Corp".into()),
                            projects: vec!["Project Alpha".into(), "Project Beta".into()],
                            extra: HashMap::new(),
                        }),
                        relationships: vec![],
                        custom_fields: None,
                        extra: HashMap::new(),
                    }),
                    prose: Some(ProseProfile {
                        user_profile: Some("Alice is a senior engineer who prefers...".into()),
                        extra: HashMap::new(),
                    }),
                    source_format: Some("openclaw".into()),
                    raw_source: None,
                    extra: HashMap::new(),
                },
                extra: HashMap::new(),
            }],
            extra: HashMap::new(),
        }
    }

    #[test]
    fn principals_round_trip() {
        let doc = sample_principals_document();
        let json = serde_json::to_string_pretty(&doc).unwrap();
        let deserialized: PrincipalsDocument = serde_json::from_str(&json).unwrap();
        assert_eq!(doc, deserialized);
    }

    #[test]
    fn principals_empty() {
        let doc = PrincipalsDocument {
            principals: vec![],
            extra: HashMap::new(),
        };

        let json = serde_json::to_string(&doc).unwrap();
        let deserialized: PrincipalsDocument = serde_json::from_str(&json).unwrap();
        assert_eq!(doc, deserialized);
    }

    #[test]
    fn principal_type_forward_compatible() {
        let json = serde_json::json!({
            "id": "550e8400-e29b-41d4-a716-446655440000",
            "principal_type": "organization",
            "profile": {
                "id": "550e8400-e29b-41d4-a716-446655440001",
                "agent_id": "550e8400-e29b-41d4-a716-446655440002",
                "principal_id": "550e8400-e29b-41d4-a716-446655440000",
                "version": 1,
                "updated_at": "2026-01-01T00:00:00Z"
            }
        });

        let principal: Principal = serde_json::from_value(json).unwrap();
        assert_eq!(
            principal.principal_type,
            PrincipalType::Unknown("organization".into())
        );

        let value = serde_json::to_value(&principal).unwrap();
        assert_eq!(value["principal_type"], "organization");
    }

    #[test]
    fn principal_agent_type() {
        let managing_agent_id = Uuid::new_v4();
        let principal = Principal {
            id: Uuid::new_v4(),
            principal_type: PrincipalType::Agent,
            agent_id: Some(managing_agent_id),
            profile: PrincipalProfile {
                id: Uuid::new_v4(),
                agent_id: Uuid::new_v4(),
                principal_id: Uuid::new_v4(),
                version: 1,
                updated_at: Utc::now(),
                structured: None,
                prose: None,
                source_format: None,
                raw_source: None,
                extra: HashMap::new(),
            },
            extra: HashMap::new(),
        };

        let json = serde_json::to_string(&principal).unwrap();
        let deserialized: Principal = serde_json::from_str(&json).unwrap();
        assert_eq!(principal, deserialized);
        assert_eq!(deserialized.agent_id, Some(managing_agent_id));
    }

    #[test]
    fn principals_unknown_fields_preserved() {
        let json = serde_json::json!({
            "principals": [{
                "id": "550e8400-e29b-41d4-a716-446655440000",
                "principal_type": "human",
                "profile": {
                    "id": "550e8400-e29b-41d4-a716-446655440001",
                    "agent_id": "550e8400-e29b-41d4-a716-446655440002",
                    "principal_id": "550e8400-e29b-41d4-a716-446655440000",
                    "version": 1,
                    "updated_at": "2026-01-01T00:00:00Z",
                    "structured": {
                        "name": "Alice",
                        "future_profile_field": 42
                    },
                    "future_profile_top": true
                },
                "future_principal_field": "kept"
            }],
            "future_doc_field": "also kept"
        });

        let doc: PrincipalsDocument = serde_json::from_value(json).unwrap();

        assert_eq!(
            doc.extra.get("future_doc_field"),
            Some(&serde_json::json!("also kept"))
        );
        assert_eq!(
            doc.principals[0].extra.get("future_principal_field"),
            Some(&serde_json::json!("kept"))
        );
        assert_eq!(
            doc.principals[0].profile.extra.get("future_profile_top"),
            Some(&serde_json::json!(true))
        );
        let structured = doc.principals[0].profile.structured.as_ref().unwrap();
        assert_eq!(
            structured.extra.get("future_profile_field"),
            Some(&serde_json::json!(42))
        );

        // Round-trip
        let serialized = serde_json::to_value(&doc).unwrap();
        assert_eq!(serialized["future_doc_field"], "also kept");
        assert_eq!(serialized["principals"][0]["future_principal_field"], "kept");
    }

    #[test]
    fn communication_preferences_round_trip() {
        let prefs = CommunicationPreferences {
            tone: Some("friendly".into()),
            response_length: Some("medium".into()),
            formatting: None,
            extra: HashMap::new(),
        };

        let json = serde_json::to_string(&prefs).unwrap();
        let deserialized: CommunicationPreferences = serde_json::from_str(&json).unwrap();
        assert_eq!(prefs, deserialized);
    }
}