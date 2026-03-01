//! Parse ZeroClaw identity into an ALF `Identity`.
//!
//! Supports two formats:
//! - **OpenClaw** (default): reads `SOUL.md`, `IDENTITY.md`, `AGENTS.md`
//! - **AIEOS**: reads a JSON file or inline JSON from `config.toml`
//!
//! The format is determined by `[identity].format` in `config.toml`.

use std::collections::HashMap;
use std::fs;
use std::path::Path;

use anyhow::{Context, Result};
use chrono::Utc;
use uuid::Uuid;

use alf_core::{
    Identity, Linguistics, Names, ProseIdentity, Psychology, StructuredIdentity,
};

use crate::config_parser::{IdentityFormat, ZeroClawConfig};

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Parse identity from a ZeroClaw workspace.
///
/// Returns `None` if no identity files are found.
pub fn parse_identity(
    workspace: &Path,
    config: &ZeroClawConfig,
    agent_id: Uuid,
) -> Result<Option<Identity>> {
    match config.identity_format {
        IdentityFormat::Aieos => parse_aieos_identity(workspace, config, agent_id),
        IdentityFormat::OpenClaw => parse_openclaw_identity(workspace, agent_id),
    }
}

/// Extract the agent's display name from workspace files.
///
/// Checks AIEOS names first, then SOUL.md H1, then IDENTITY.md H1.
pub fn detect_agent_name(workspace: &Path, config: &ZeroClawConfig) -> String {
    // Try AIEOS name
    if config.identity_format == IdentityFormat::Aieos {
        if let Some(name) = extract_aieos_name(workspace, config) {
            return name;
        }
    }

    // Try SOUL.md H1
    if let Ok(content) = fs::read_to_string(workspace.join("SOUL.md")) {
        if let Some(name) = extract_h1(&content) {
            return name;
        }
    }

    // Try IDENTITY.md H1
    if let Ok(content) = fs::read_to_string(workspace.join("IDENTITY.md")) {
        if let Some(name) = extract_h1(&content) {
            return name;
        }
    }

    // Fallback to directory name
    workspace
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("Unknown")
        .to_string()
}

// ---------------------------------------------------------------------------
// OpenClaw-format identity
// ---------------------------------------------------------------------------

fn parse_openclaw_identity(workspace: &Path, agent_id: Uuid) -> Result<Option<Identity>> {
    let soul = read_optional(workspace, "SOUL.md");
    let identity_md = read_optional(workspace, "IDENTITY.md");
    let agents = read_optional(workspace, "AGENTS.md");

    if soul.is_none() && identity_md.is_none() && agents.is_none() {
        return Ok(None);
    }

    let name = soul
        .as_deref()
        .and_then(|c| extract_h1(c))
        .or_else(|| identity_md.as_deref().and_then(|c| extract_h1(c)))
        .unwrap_or_else(|| "Unknown".to_string());

    Ok(Some(Identity {
        id: Uuid::new_v4(),
        agent_id,
        version: 1,
        updated_at: Utc::now(),
        structured: Some(StructuredIdentity {
            names: Some(Names {
                primary: name,
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
        }),
        prose: Some(ProseIdentity {
            soul,
            identity_profile: identity_md,
            operating_instructions: agents,
            custom_blocks: HashMap::new(),
            extra: HashMap::new(),
        }),
        source_format: Some("openclaw".to_string()),
        raw_source: None,
        extra: HashMap::new(),
    }))
}

// ---------------------------------------------------------------------------
// AIEOS-format identity
// ---------------------------------------------------------------------------

fn parse_aieos_identity(
    workspace: &Path,
    config: &ZeroClawConfig,
    agent_id: Uuid,
) -> Result<Option<Identity>> {
    // Load AIEOS JSON from file or inline
    let raw_json = load_aieos_json(workspace, config)?;
    let raw_json = match raw_json {
        Some(j) => j,
        None => {
            // Fallback to OpenClaw format if AIEOS source isn't available
            return parse_openclaw_identity(workspace, agent_id);
        }
    };

    let val: serde_json::Value = serde_json::from_str(&raw_json)
        .context("Failed to parse AIEOS identity JSON")?;

    let identity_obj = val.get("identity").unwrap_or(&val);

    // Extract names
    let names = identity_obj.get("names").and_then(|n| {
        let primary = n.get("first")?.as_str()?.to_string();
        let nickname = n.get("nickname").and_then(|v| v.as_str()).map(|s| s.to_string());
        let full = n.get("full").and_then(|v| v.as_str()).map(|s| s.to_string());
        Some(Names {
            primary,
            nickname,
            full,
            extra: HashMap::new(),
        })
    });

    // Extract psychology
    let psychology = identity_obj.get("psychology").and_then(|p| {
        let mut psych = Psychology {
            neural_matrix: HashMap::new(),
            personality_traits: None,
            moral_alignment: None,
            mbti: None,
            extra: HashMap::new(),
        };

        if let Some(nm) = p.get("neural_matrix").and_then(|v| v.as_object()) {
            for (k, v) in nm {
                if let Some(f) = v.as_f64() {
                    psych.neural_matrix.insert(k.clone(), f);
                }
            }
        }

        psych.mbti = p
            .get("traits")
            .and_then(|t| t.get("mbti"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        psych.moral_alignment = p
            .get("moral_compass")
            .and_then(|mc| mc.get("alignment"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        Some(psych)
    });

    // Extract linguistics
    let linguistics = identity_obj.get("linguistics").and_then(|l| {
        let text_style = l.get("text_style");
        Some(Linguistics {
            formality_level: text_style
                .and_then(|ts| ts.get("formality_level"))
                .and_then(|v| v.as_f64()),
            verbosity: None,
            humor_level: None,
            slang_usage: text_style
                .and_then(|ts| ts.get("slang_usage"))
                .and_then(|v| v.as_bool()),
            preferred_language: None,
            idiolect: None,
            extra: HashMap::new(),
        })
    });

    // Extract goals from motivations
    let goals: Vec<String> = identity_obj
        .get("motivations")
        .and_then(|m| m.get("core_drive"))
        .and_then(|v| v.as_str())
        .map(|s| vec![s.to_string()])
        .unwrap_or_default();

    // Also load any OpenClaw prose files that may coexist
    let soul = read_optional(workspace, "SOUL.md");
    let identity_md = read_optional(workspace, "IDENTITY.md");
    let agents = read_optional(workspace, "AGENTS.md");

    let prose = if soul.is_some() || identity_md.is_some() || agents.is_some() {
        Some(ProseIdentity {
            soul,
            identity_profile: identity_md,
            operating_instructions: agents,
            custom_blocks: HashMap::new(),
            extra: HashMap::new(),
        })
    } else {
        None
    };

    let agent_name = names
        .as_ref()
        .map(|n| n.primary.clone())
        .unwrap_or_else(|| "Unknown".to_string());

    // Store remaining AIEOS fields in aieos_extensions
    let aieos_extensions = {
        let mut extensions = serde_json::Map::new();
        for (key, value) in identity_obj.as_object().into_iter().flatten() {
            if !["names", "psychology", "linguistics", "motivations"].contains(&key.as_str()) {
                extensions.insert(key.clone(), value.clone());
            }
        }
        if extensions.is_empty() {
            None
        } else {
            Some(serde_json::Value::Object(extensions))
        }
    };

    Ok(Some(Identity {
        id: Uuid::new_v4(),
        agent_id,
        version: 1,
        updated_at: Utc::now(),
        structured: Some(StructuredIdentity {
            names: Some(Names {
                primary: agent_name,
                nickname: names.as_ref().and_then(|n| n.nickname.clone()),
                full: names.as_ref().and_then(|n| n.full.clone()),
                extra: HashMap::new(),
            }),
            role: None,
            goals,
            psychology,
            linguistics,
            capabilities: Vec::new(),
            sub_agents: Vec::new(),
            aieos_extensions,
            extra: HashMap::new(),
        }),
        prose,
        source_format: Some("aieos".to_string()),
        raw_source: Some(serde_json::from_str(&raw_json).unwrap_or(serde_json::Value::Null)),
        extra: HashMap::new(),
    }))
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn load_aieos_json(workspace: &Path, config: &ZeroClawConfig) -> Result<Option<String>> {
    // Try file path first
    if let Some(ref path_str) = config.aieos_path {
        let path = if Path::new(path_str).is_absolute() {
            Path::new(path_str).to_path_buf()
        } else {
            workspace.join(path_str)
        };
        if path.is_file() {
            return Ok(Some(
                fs::read_to_string(&path)
                    .with_context(|| format!("Failed to read AIEOS file: {}", path.display()))?,
            ));
        }
    }

    // Try inline JSON
    if let Some(ref inline) = config.aieos_inline {
        return Ok(Some(inline.clone()));
    }

    Ok(None)
}

fn extract_aieos_name(workspace: &Path, config: &ZeroClawConfig) -> Option<String> {
    let json = load_aieos_json(workspace, config).ok()??;
    let val: serde_json::Value = serde_json::from_str(&json).ok()?;
    let identity = val.get("identity").unwrap_or(&val);
    identity
        .get("names")?
        .get("first")?
        .as_str()
        .map(|s| s.to_string())
}

fn read_optional(workspace: &Path, filename: &str) -> Option<String> {
    let path = workspace.join(filename);
    let content = fs::read_to_string(&path).ok()?;
    if content.trim().is_empty() {
        None
    } else {
        Some(content)
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

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn default_config() -> ZeroClawConfig {
        ZeroClawConfig {
            memory_backend: crate::config_parser::MemoryBackend::Sqlite,
            auto_save: true,
            embedding_provider: "none".into(),
            vector_weight: 0.7,
            keyword_weight: 0.3,
            identity_format: IdentityFormat::OpenClaw,
            aieos_path: None,
            aieos_inline: None,
            secrets_encrypt: true,
            credential_hints: Vec::new(),
            raw_toml: String::new(),
        }
    }

    #[test]
    fn openclaw_format_all_files() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path();
        fs::write(ws.join("SOUL.md"), "# Nova\n\nA helpful assistant.\n").unwrap();
        fs::write(ws.join("IDENTITY.md"), "# Nova Identity\n\nDetails.\n").unwrap();
        fs::write(ws.join("AGENTS.md"), "# Operating Instructions\n\nBe helpful.\n").unwrap();

        let config = default_config();
        let id = parse_identity(ws, &config, Uuid::new_v4()).unwrap().unwrap();

        assert_eq!(id.source_format.as_deref(), Some("openclaw"));
        let prose = id.prose.unwrap();
        assert!(prose.soul.unwrap().contains("Nova"));
        assert!(prose.identity_profile.unwrap().contains("Details"));
        assert!(prose.operating_instructions.unwrap().contains("Be helpful"));
        let structured = id.structured.unwrap();
        assert_eq!(structured.names.unwrap().primary, "Nova");
    }

    #[test]
    fn openclaw_format_soul_only() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path();
        fs::write(ws.join("SOUL.md"), "# Agent\n\nMinimal.\n").unwrap();

        let config = default_config();
        let id = parse_identity(ws, &config, Uuid::new_v4()).unwrap().unwrap();
        assert!(id.prose.unwrap().identity_profile.is_none());
    }

    #[test]
    fn no_identity_files_returns_none() {
        let tmp = tempfile::tempdir().unwrap();
        let config = default_config();
        let result = parse_identity(tmp.path(), &config, Uuid::new_v4()).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn aieos_from_file() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path();
        let aieos = r#"{
            "identity": {
                "names": { "first": "Nova", "nickname": "N" },
                "psychology": {
                    "neural_matrix": { "creativity": 0.9, "logic": 0.8 },
                    "traits": { "mbti": "ENTP" },
                    "moral_compass": { "alignment": "Chaotic Good" }
                },
                "linguistics": {
                    "text_style": { "formality_level": 0.2, "slang_usage": true }
                },
                "motivations": {
                    "core_drive": "Push boundaries and explore possibilities"
                }
            }
        }"#;
        fs::write(ws.join("identity.json"), aieos).unwrap();

        let mut config = default_config();
        config.identity_format = IdentityFormat::Aieos;
        config.aieos_path = Some("identity.json".into());

        let id = parse_identity(ws, &config, Uuid::new_v4()).unwrap().unwrap();
        assert_eq!(id.source_format.as_deref(), Some("aieos"));

        let s = id.structured.unwrap();
        assert_eq!(s.names.as_ref().unwrap().primary, "Nova");
        assert_eq!(s.names.as_ref().unwrap().nickname.as_deref(), Some("N"));
        assert_eq!(s.goals, vec!["Push boundaries and explore possibilities"]);

        let psych = s.psychology.unwrap();
        assert_eq!(psych.neural_matrix.get("creativity"), Some(&0.9));
        assert_eq!(psych.mbti.as_deref(), Some("ENTP"));
        assert_eq!(psych.moral_alignment.as_deref(), Some("Chaotic Good"));

        let ling = s.linguistics.unwrap();
        assert_eq!(ling.formality_level, Some(0.2));
        assert_eq!(ling.slang_usage, Some(true));

        assert!(id.raw_source.is_some());
    }

    #[test]
    fn aieos_inline() {
        let tmp = tempfile::tempdir().unwrap();
        let mut config = default_config();
        config.identity_format = IdentityFormat::Aieos;
        config.aieos_inline = Some(r#"{"identity":{"names":{"first":"Inline"}}}"#.into());

        let id = parse_identity(tmp.path(), &config, Uuid::new_v4()).unwrap().unwrap();
        let s = id.structured.unwrap();
        assert_eq!(s.names.unwrap().primary, "Inline");
    }

    #[test]
    fn aieos_fallback_to_openclaw() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path();
        fs::write(ws.join("SOUL.md"), "# Fallback Agent\n\nHello.\n").unwrap();

        let mut config = default_config();
        config.identity_format = IdentityFormat::Aieos;
        // No aieos_path or aieos_inline — should fall back to openclaw files

        let id = parse_identity(ws, &config, Uuid::new_v4()).unwrap().unwrap();
        assert_eq!(id.source_format.as_deref(), Some("openclaw"));
    }

    #[test]
    fn detect_name_aieos() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path();
        fs::write(ws.join("identity.json"), r#"{"identity":{"names":{"first":"AiName"}}}"#).unwrap();

        let mut config = default_config();
        config.identity_format = IdentityFormat::Aieos;
        config.aieos_path = Some("identity.json".into());

        assert_eq!(detect_agent_name(ws, &config), "AiName");
    }
}