//! Identity layer types for the ALF format.
//!
//! Matches `identity.schema.json`. Dual representation — structured fields
//! (machine-readable) and prose blocks (LLM-interpreted). See §3.2 of the
//! ALF specification.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use uuid::Uuid;

// forward_compatible_enum! is available via #[macro_use] on mod memory in lib.rs

// ---------------------------------------------------------------------------
// Enums
// ---------------------------------------------------------------------------

forward_compatible_enum! {
    /// Portability annotation for a capability (§3.2.6).
    pub enum CapabilityPortability {
        Intrinsic     => "intrinsic",
        HostDependent => "host_dependent",
    }
}

forward_compatible_enum! {
    /// Status of a sub-agent (§3.2.4).
    pub enum SubAgentStatus {
        Active      => "active",
        Inactive    => "inactive",
        Unavailable => "unavailable",
    }
}

// ---------------------------------------------------------------------------
// Top-level Identity
// ---------------------------------------------------------------------------

/// Layer 2: Identity and persona (§3.2).
///
/// This is the top-level JSON object stored in the identity file
/// referenced by the manifest.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Identity {
    /// Unique identifier for this identity document.
    pub id: Uuid,

    /// The agent this identity belongs to.
    pub agent_id: Uuid,

    /// Identity version number. Incremented on every change.
    pub version: u32,

    /// When this version was created.
    pub updated_at: DateTime<Utc>,

    /// Machine-readable identity fields.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub structured: Option<StructuredIdentity>,

    /// Rich prose blocks interpreted by the LLM at runtime.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prose: Option<ProseIdentity>,

    /// The runtime that originally defined this identity.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_format: Option<String>,

    /// Original runtime-specific identity for lossless round-trip.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub raw_source: Option<serde_json::Value>,

    /// Unknown fields preserved for forward compatibility.
    #[serde(flatten)]
    pub extra: HashMap<String, serde_json::Value>,
}

// ---------------------------------------------------------------------------
// Structured Identity
// ---------------------------------------------------------------------------

/// Machine-readable identity fields (§3.2.2, §3.2.6).
///
/// Based on AIEOS v1.1 as a compatible superset.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StructuredIdentity {
    /// Agent name variants.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub names: Option<Names>,

    /// Short description of the agent's role.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,

    /// Agent's goals and motivations.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub goals: Vec<String>,

    /// Personality and behavioral traits.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub psychology: Option<Psychology>,

    /// Language style and output behavior.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub linguistics: Option<Linguistics>,

    /// Agent capabilities for task routing and tool discovery.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub capabilities: Vec<Capability>,

    /// Roster of sub-agents this agent manages (§3.2.4).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sub_agents: Vec<SubAgent>,

    /// Passthrough for AIEOS fields not promoted to first-class (§3.2.6).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub aieos_extensions: Option<serde_json::Value>,

    /// Unknown fields preserved for forward compatibility.
    #[serde(flatten)]
    pub extra: HashMap<String, serde_json::Value>,
}

/// Agent name variants.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Names {
    /// The agent's primary/display name.
    pub primary: String,

    /// Short nickname.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub nickname: Option<String>,

    /// Full formal name.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub full: Option<String>,

    /// Unknown fields preserved for forward compatibility.
    #[serde(flatten)]
    pub extra: HashMap<String, serde_json::Value>,
}

// ---------------------------------------------------------------------------
// Psychology
// ---------------------------------------------------------------------------

/// Personality and behavioral traits (§3.2.6).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Psychology {
    /// AIEOS neural matrix — trait names to float scores (0.0–1.0).
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub neural_matrix: HashMap<String, f64>,

    /// Personality framework scores.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub personality_traits: Option<PersonalityTraits>,

    /// Moral alignment label (e.g., `"Neutral Good"`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub moral_alignment: Option<String>,

    /// MBTI type (e.g., `"ENTP"`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mbti: Option<String>,

    /// Unknown fields preserved for forward compatibility.
    #[serde(flatten)]
    pub extra: HashMap<String, serde_json::Value>,
}

/// Personality framework scores (§3.2.6).
///
/// OCEAN/Big Five is the promoted framework.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PersonalityTraits {
    /// Which personality framework (e.g., `"OCEAN"`).
    pub framework: String,

    /// Framework-specific trait scores (e.g., openness → 0.8).
    pub scores: HashMap<String, f64>,

    /// Unknown fields preserved for forward compatibility.
    #[serde(flatten)]
    pub extra: HashMap<String, serde_json::Value>,
}

// ---------------------------------------------------------------------------
// Linguistics
// ---------------------------------------------------------------------------

/// Language style and output behavior (§3.2.6).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Linguistics {
    /// 0.0 (very casual) to 1.0 (very formal).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub formality_level: Option<f64>,

    /// 0.0 (very terse) to 1.0 (very verbose).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub verbosity: Option<f64>,

    /// 0.0 (no humor) to 1.0 (very humorous).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub humor_level: Option<f64>,

    /// Whether the agent uses slang.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub slang_usage: Option<bool>,

    /// BCP-47 language tag (e.g., `"en"`, `"ja"`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preferred_language: Option<String>,

    /// Agent-specific speech patterns.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub idiolect: Option<Idiolect>,

    /// Unknown fields preserved for forward compatibility.
    #[serde(flatten)]
    pub extra: HashMap<String, serde_json::Value>,
}

/// Agent-specific speech patterns.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Idiolect {
    /// Catchphrases the agent uses.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub catchphrases: Vec<String>,

    /// Verbal tics.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub verbal_tics: Vec<String>,

    /// Words the agent avoids.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub avoided_words: Vec<String>,

    /// Unknown fields preserved for forward compatibility.
    #[serde(flatten)]
    pub extra: HashMap<String, serde_json::Value>,
}

// ---------------------------------------------------------------------------
// Capabilities
// ---------------------------------------------------------------------------

/// A single agent capability (§3.2.6).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Capability {
    /// Capability identifier (e.g., `"web_search"`, `"code_generation"`).
    pub name: String,

    /// What this capability does.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,

    /// Priority ranking. 1 = highest.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub priority: Option<u32>,

    /// Whether this capability is intrinsic or host-dependent.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub portability: Option<CapabilityPortability>,

    /// What the host must provide for host-dependent capabilities.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub host_requirements: Option<String>,

    /// IDs of credentials (from Layer 4) required by this capability.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub credential_ids: Vec<Uuid>,

    /// Unknown fields preserved for forward compatibility.
    #[serde(flatten)]
    pub extra: HashMap<String, serde_json::Value>,
}

// ---------------------------------------------------------------------------
// Sub-agents
// ---------------------------------------------------------------------------

/// A sub-agent in the managing agent's roster (§3.2.4).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SubAgent {
    /// References the sub-agent's own `.alf` file if it has persistent state.
    /// `None` for ephemeral sub-agents.
    pub agent_id: Option<Uuid>,

    /// Display name of the sub-agent.
    pub name: String,

    /// What the sub-agent can do.
    pub capabilities: Vec<Capability>,

    /// Sub-agent lifecycle status.
    pub status: SubAgentStatus,

    /// Brief summary of what this sub-agent does.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,

    /// Model hints for the sub-agent (§3.2.5).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_hints: Option<SubAgentModelHints>,

    /// Prose guidance for when and how to use this sub-agent.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub routing_hints: Option<String>,

    /// When the managing agent last delegated a task to this sub-agent.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_invoked_at: Option<DateTime<Utc>>,

    /// Managing agent's observations about strengths and weaknesses.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub performance_notes: Option<String>,

    /// Unknown fields preserved for forward compatibility.
    #[serde(flatten)]
    pub extra: HashMap<String, serde_json::Value>,
}

/// Model hints for a sub-agent.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SubAgentModelHints {
    /// Model the sub-agent spent most of its life on.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub primary_model: Option<String>,

    /// Most recently used model.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_model: Option<String>,

    /// Unknown fields preserved for forward compatibility.
    #[serde(flatten)]
    pub extra: HashMap<String, serde_json::Value>,
}

// ---------------------------------------------------------------------------
// Prose Identity
// ---------------------------------------------------------------------------

/// Rich prose identity blocks interpreted by the LLM at runtime (§3.2.1).
///
/// Based on OpenClaw's SOUL.md / IDENTITY.md pattern.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProseIdentity {
    /// Full content of SOUL.md or equivalent.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub soul: Option<String>,

    /// Full content of AGENTS.md or equivalent.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub operating_instructions: Option<String>,

    /// Full content of IDENTITY.md or equivalent.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub identity_profile: Option<String>,

    /// Additional prose blocks keyed by name.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub custom_blocks: HashMap<String, String>,

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

    fn sample_identity() -> Identity {
        let now = Utc.with_ymd_and_hms(2026, 2, 15, 12, 0, 0).unwrap();
        let agent_id = Uuid::new_v4();

        Identity {
            id: Uuid::new_v4(),
            agent_id,
            version: 3,
            updated_at: now,
            structured: Some(StructuredIdentity {
                names: Some(Names {
                    primary: "Alf".into(),
                    nickname: Some("Alfie".into()),
                    full: Some("Agent Life Format Assistant".into()),
                    extra: HashMap::new(),
                }),
                role: Some("Personal development assistant".into()),
                goals: vec![
                    "Help the user accomplish their goals".into(),
                    "Maintain long-term context".into(),
                ],
                psychology: Some(Psychology {
                    neural_matrix: HashMap::from([
                        ("curiosity".into(), 0.9),
                        ("empathy".into(), 0.85),
                    ]),
                    personality_traits: Some(PersonalityTraits {
                        framework: "OCEAN".into(),
                        scores: HashMap::from([
                            ("openness".into(), 0.85),
                            ("conscientiousness".into(), 0.9),
                            ("extraversion".into(), 0.6),
                            ("agreeableness".into(), 0.8),
                            ("neuroticism".into(), 0.2),
                        ]),
                        extra: HashMap::new(),
                    }),
                    moral_alignment: Some("Neutral Good".into()),
                    mbti: Some("INFJ".into()),
                    extra: HashMap::new(),
                }),
                linguistics: Some(Linguistics {
                    formality_level: Some(0.4),
                    verbosity: Some(0.6),
                    humor_level: Some(0.3),
                    slang_usage: Some(false),
                    preferred_language: Some("en".into()),
                    idiolect: Some(Idiolect {
                        catchphrases: vec!["Let me think about that...".into()],
                        verbal_tics: vec![],
                        avoided_words: vec!["honestly".into(), "genuinely".into()],
                        extra: HashMap::new(),
                    }),
                    extra: HashMap::new(),
                }),
                capabilities: vec![
                    Capability {
                        name: "web_search".into(),
                        description: Some("Search the web for current information".into()),
                        priority: Some(1),
                        portability: Some(CapabilityPortability::Intrinsic),
                        host_requirements: None,
                        credential_ids: vec![],
                        extra: HashMap::new(),
                    },
                    Capability {
                        name: "docker_management".into(),
                        description: Some("Manage Docker containers".into()),
                        priority: Some(3),
                        portability: Some(CapabilityPortability::HostDependent),
                        host_requirements: Some("Docker Engine accessible via CLI".into()),
                        credential_ids: vec![],
                        extra: HashMap::new(),
                    },
                ],
                sub_agents: vec![SubAgent {
                    agent_id: None,
                    name: "Research Bot".into(),
                    capabilities: vec![Capability {
                        name: "deep_research".into(),
                        description: None,
                        priority: None,
                        portability: None,
                        host_requirements: None,
                        credential_ids: vec![],
                        extra: HashMap::new(),
                    }],
                    status: SubAgentStatus::Active,
                    description: Some("Handles long-running research tasks".into()),
                    model_hints: Some(SubAgentModelHints {
                        primary_model: Some("anthropic/claude-sonnet-4-20250514".into()),
                        last_model: None,
                        extra: HashMap::new(),
                    }),
                    routing_hints: Some("Use for tasks requiring 10+ sources".into()),
                    last_invoked_at: Some(now),
                    performance_notes: None,
                    extra: HashMap::new(),
                }],
                aieos_extensions: Some(serde_json::json!({
                    "physicality": { "appearance": "holographic" },
                    "origin": { "creation_date": "2025-06-15" }
                })),
                extra: HashMap::new(),
            }),
            prose: Some(ProseIdentity {
                soul: Some("You are a thoughtful assistant...".into()),
                operating_instructions: Some("Follow these rules...".into()),
                identity_profile: Some("Alf was created to help...".into()),
                custom_blocks: HashMap::from([
                    ("boot_checklist".into(), "1. Load memory\n2. Check goals".into()),
                ]),
                extra: HashMap::new(),
            }),
            source_format: Some("openclaw".into()),
            raw_source: None,
            extra: HashMap::new(),
        }
    }

    #[test]
    fn identity_round_trip() {
        let identity = sample_identity();
        let json = serde_json::to_string_pretty(&identity).unwrap();
        let deserialized: Identity = serde_json::from_str(&json).unwrap();
        assert_eq!(identity, deserialized);
    }

    #[test]
    fn identity_minimal() {
        let now = Utc::now();
        let identity = Identity {
            id: Uuid::new_v4(),
            agent_id: Uuid::new_v4(),
            version: 1,
            updated_at: now,
            structured: None,
            prose: None,
            source_format: None,
            raw_source: None,
            extra: HashMap::new(),
        };

        let value = serde_json::to_value(&identity).unwrap();
        let obj = value.as_object().unwrap();
        assert!(obj.contains_key("id"));
        assert!(obj.contains_key("version"));
        assert!(!obj.contains_key("structured"));
        assert!(!obj.contains_key("prose"));
        assert!(!obj.contains_key("source_format"));

        let json = serde_json::to_string(&identity).unwrap();
        let deserialized: Identity = serde_json::from_str(&json).unwrap();
        assert_eq!(identity, deserialized);
    }

    #[test]
    fn identity_unknown_fields_preserved() {
        let json = serde_json::json!({
            "id": "550e8400-e29b-41d4-a716-446655440000",
            "agent_id": "550e8400-e29b-41d4-a716-446655440001",
            "version": 1,
            "updated_at": "2026-01-01T00:00:00Z",
            "structured": {
                "names": { "primary": "Test", "honorific": "Dr." },
                "future_structured_field": true
            },
            "future_identity_field": "preserved"
        });

        let identity: Identity = serde_json::from_value(json).unwrap();
        assert_eq!(
            identity.extra.get("future_identity_field"),
            Some(&serde_json::json!("preserved"))
        );
        let structured = identity.structured.unwrap();
        assert_eq!(
            structured.extra.get("future_structured_field"),
            Some(&serde_json::json!(true))
        );
        assert_eq!(
            structured.names.unwrap().extra.get("honorific"),
            Some(&serde_json::json!("Dr."))
        );
    }

    #[test]
    fn capability_portability_forward_compatible() {
        let cap: Capability = serde_json::from_value(serde_json::json!({
            "name": "quantum_compute",
            "portability": "cloud_only"
        }))
        .unwrap();

        assert_eq!(
            cap.portability,
            Some(CapabilityPortability::Unknown("cloud_only".into()))
        );

        let value = serde_json::to_value(&cap).unwrap();
        assert_eq!(value["portability"], "cloud_only");
    }

    #[test]
    fn sub_agent_status_forward_compatible() {
        let sub: SubAgent = serde_json::from_value(serde_json::json!({
            "agent_id": null,
            "name": "Test",
            "capabilities": [],
            "status": "deprecated"
        }))
        .unwrap();

        assert_eq!(
            sub.status,
            SubAgentStatus::Unknown("deprecated".into())
        );

        let value = serde_json::to_value(&sub).unwrap();
        assert_eq!(value["status"], "deprecated");
    }

    #[test]
    fn personality_traits_round_trip() {
        let traits = PersonalityTraits {
            framework: "OCEAN".into(),
            scores: HashMap::from([
                ("openness".into(), 0.85),
                ("neuroticism".into(), 0.2),
            ]),
            extra: HashMap::new(),
        };

        let json = serde_json::to_string(&traits).unwrap();
        let deserialized: PersonalityTraits = serde_json::from_str(&json).unwrap();
        assert_eq!(traits, deserialized);
    }

    #[test]
    fn prose_custom_blocks_round_trip() {
        let prose = ProseIdentity {
            soul: None,
            operating_instructions: None,
            identity_profile: None,
            custom_blocks: HashMap::from([
                ("boot_checklist".into(), "Step 1\nStep 2".into()),
                ("heartbeat_checklist".into(), "Check memory".into()),
            ]),
            extra: HashMap::new(),
        };

        let json = serde_json::to_string(&prose).unwrap();
        let deserialized: ProseIdentity = serde_json::from_str(&json).unwrap();
        assert_eq!(prose, deserialized);
    }

    #[test]
    fn aieos_extensions_passthrough() {
        let structured = StructuredIdentity {
            names: None,
            role: None,
            goals: vec![],
            psychology: None,
            linguistics: None,
            capabilities: vec![],
            sub_agents: vec![],
            aieos_extensions: Some(serde_json::json!({
                "physicality": {
                    "appearance": "tall, dark hair",
                    "voice": "baritone"
                },
                "dnd_alignment": "Chaotic Good",
                "mbti_detailed": { "type": "ENTP", "confidence": 0.8 }
            })),
            extra: HashMap::new(),
        };

        let json = serde_json::to_string(&structured).unwrap();
        let deserialized: StructuredIdentity = serde_json::from_str(&json).unwrap();
        assert_eq!(structured, deserialized);

        // Extensions survive intact
        let ext = deserialized.aieos_extensions.unwrap();
        assert_eq!(ext["dnd_alignment"], "Chaotic Good");
        assert_eq!(ext["physicality"]["voice"], "baritone");
    }
}